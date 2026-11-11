use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_loop::output_obligation::OutputObligationCheck;

use super::super::AgentDaemon;
use super::compactor_failure;
use crate::inference::admission::{self, CallKind};
use crate::inference::compaction::{ReductionEngine, ReductionOptions};
use crate::inference::llm::message::Message;
use crate::inference::resolved::MaxTurnsProvenance;
use crate::runtime::agent::loop_stream::{
    AggregateTokenBudget, FoldedPrompt, LoopConfig, LoopReplayInput, TaggedMessage,
    TurnCompactionError, TurnCompactionOutcome, TurnCompactionRequest,
};
use crate::runtime::agent::output_obligation::OutputObligationGate;
use crate::tools::call::hook::DefraSessionHook;
use crate::turn::provider_input::budget::ContextCalibration;
use crate::turn::provider_input::ProviderInputProfile;
use gents_protocol::row::AgentRequest;

#[derive(Clone, Copy)]
pub(super) struct InferencePreparation<'a> {
    pub(super) request: &'a AgentRequest,
    pub(super) lifecycle: &'a crate::request::lifecycle::RequestLifecycle,
    pub(super) request_deadline: Option<DateTime<Utc>>,
    pub(super) inference_observation:
        &'a Option<Arc<crate::inference::inference_observation::InferenceObservationGuard>>,
    pub(super) effective_seed: Option<i64>,
    pub(super) context_calibration: &'a ContextCalibration,
    pub(super) request_context_message: &'a Option<Message>,
    pub(super) resume_from_history: bool,
    pub(super) consumed_prompts: &'a Vec<FoldedPrompt>,
    pub(super) folded_prompts: &'a Vec<FoldedPrompt>,
    pub(super) history: &'a [TaggedMessage],
    pub(super) wake_notifications: &'a [TaggedMessage],
}

pub(super) struct PreparedInference<M> {
    pub(super) hook: DefraSessionHook,
    pub(super) persistence_hook: DefraSessionHook,
    pub(super) model: M,
    pub(super) loop_config: LoopConfig,
    pub(super) loop_history: Vec<TaggedMessage>,
    pub(super) loop_prompt: TaggedMessage,
    pub(super) max_turns_provenance: MaxTurnsProvenance,
}

#[derive(Clone)]
struct TurnCompaction {
    compactor: Arc<dyn ReductionEngine>,
    options: ReductionOptions,
    node: Arc<defra_node::Node>,
    agent_did: String,
    signer: crate::store::identity::ManagedAgentSigner,
    request: AgentRequest,
    request_commit_cid: String,
    provider_profile: ProviderInputProfile,
    turn_context_window: usize,
}

type TurnCompactorCallback = Arc<
    dyn Fn(
            TurnCompactionRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<TurnCompactionOutcome, TurnCompactionError>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

fn turn_compactor_callback(compaction: TurnCompaction) -> TurnCompactorCallback {
    std::sync::Arc::new(
        move |compaction_request: crate::runtime::agent::loop_stream::TurnCompactionRequest| -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            crate::runtime::agent::loop_stream::TurnCompactionOutcome,
                            crate::runtime::agent::loop_stream::TurnCompactionError,
                        >,
                    > + Send,
            >,
        > { compact_turn(compaction.clone(), compaction_request) },
    )
}

fn compact_turn(
    compaction: TurnCompaction,
    compaction_request: TurnCompactionRequest,
) -> std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<TurnCompactionOutcome, TurnCompactionError>> + Send,
    >,
> {
    let TurnCompaction {
        compactor,
        mut options,
        node,
        agent_did,
        signer,
        request,
        request_commit_cid,
        provider_profile,
        turn_context_window,
    } = compaction;
    Box::pin(async move {
        let provider_view =
            crate::runtime::agent::loop_stream::provider_view_tagged(compaction_request.messages)
                .map_err(compactor_failure)?;
        options.max_compacted_prefix_messages =
            crate::runtime::agent::loop_stream::replay_compaction_prefix_bound(
                &provider_view,
                &compaction_request.required,
            )
            .map_err(crate::inference::compaction::CompactionError::from)?;
        let native_messages =
            crate::runtime::agent::loop_stream::provider_messages(provider_profile, &provider_view);
        options.keep_recent_tokens = compactor.retention_target(
            options.keep_recent_tokens,
            &native_messages,
            compaction_request.admission,
            &options,
            turn_context_window,
        )?;
        // Request-local sticky projection, not a
        // session-prefix cursor; the stronger
        // safe_to_reduce gate belongs to session compaction.
        let source_boundary = crate::session::provider_context_reduction::capture_source_boundary(
            &crate::ConfigAccess::local_with_agent_signer(
                node.clone(),
                identity::Did::new(agent_did.clone()).map_err(compactor_failure)?,
                signer.clone(),
            )
            .map_err(compactor_failure)?,
            &request.session_id,
            &request.node_did,
            request.requester_did.as_deref(),
            &request.doc_id,
            &request_commit_cid,
        )
        .await
        .map_err(compactor_failure)?;
        let (result, producer_join) = admission::scope_call_with_join(
            CallKind::Compaction,
            1,
            compactor.reduce(
                native_messages,
                turn_context_window,
                &options,
                compaction_request.admission,
            ),
        )
        .await;
        let result = result?;
        let producer_call = producer_join
            .filter(|join| matches!(join.call_kind, CallKind::Compaction))
            .map(
                |join| crate::session::provider_context_reduction::ProducerCallRef {
                    call_id: join.call_id,
                    call_seq: join.call_seq,
                },
            );
        let provider_values =
            crate::runtime::agent::loop_stream::provider_messages(provider_profile, &provider_view);
        let Some(exact) = result.exact_reduction() else {
            if result.cannot_fit() {
                return Ok(crate::runtime::agent::loop_stream::TurnCompactionOutcome::CannotFit);
            }
            if result
                .provider_messages()
                .map_err(crate::inference::compaction::CompactionError::from)?
                != provider_values
            {
                return Err(compactor_failure(
                    "repaired provider view disagrees with source-index projection",
                ));
            }
            return Ok(
                crate::runtime::agent::loop_stream::TurnCompactionOutcome::ProviderViewRepaired {
                    messages: provider_view,
                },
            );
        };
        if !exact
            .compacted_prefix
            .iter()
            .chain(exact.retained_suffix)
            .eq(provider_values.iter())
        {
            return Err(compactor_failure(
                "exact reduction split disagrees with source-index provider view",
            ));
        }
        let split = exact.compacted_prefix.len();
        let native = crate::runtime::agent::loop_stream::association_reduction(
            provider_profile,
            &provider_view,
            exact,
        )?;
        let exact = native.exact();
        let associations =
            crate::session::provider_context_reduction::ReplayAssociations::from_tagged_split(
                compaction_request.required,
                &provider_view[..split],
                &provider_view[split..],
            );
        let reduction_index = compaction_request.prior_reduction_keys.len() + 1;
        crate::session::provider_context_reduction::stage_exact(
            crate::session::provider_context_reduction::NewExactProviderContextReduction {
                agent_did: &agent_did,
                node_did: &request.node_did,
                requester_did: request.requester_did.as_deref(),
                session_id: &request.session_id,
                request_id: &request.request_id,
                request_doc_id: &request.doc_id,
                request_commit_cid: &request_commit_cid,
                reduction_index,
                turn_index: compaction_request.turn_index,
                parent_reduction_key: compaction_request
                    .prior_reduction_keys
                    .last()
                    .map(String::as_str),
                producer_call: producer_call.as_ref(),
                source_boundary: &source_boundary,
                replay_associations: &associations,
                original_tokens: result.original_token_estimate,
                compacted_tokens: result.compacted_token_estimate,
            },
            exact,
        )
        .and_then(|staged| staged.into_turn_outcome(node, signer))
        .map_err(compactor_failure)
    })
}

impl<M> AgentDaemon<M>
where
    M: crate::inference::llm::rig_compat::ProviderModel,
{
    pub(super) async fn prepare_inference(
        &self,
        preparation: InferencePreparation<'_>,
        mut replay: LoopReplayInput,
        aggregate_token_budget: Option<AggregateTokenBudget>,
    ) -> Result<PreparedInference<M>> {
        let InferencePreparation {
            request,
            lifecycle,
            request_deadline,
            effective_seed,
            context_calibration,
            request_context_message,
            resume_from_history,
            folded_prompts,
            ..
        } = preparation;
        let (hook, output_obligation_gate) = self.session_hook(preparation).await?;
        let persistence_hook = hook.clone();

        let model = (*self.model).clone();
        let mut loop_config = crate::turn::completion_factory::loop_config_for_request(
            &self.agent_config,
            self.preamble.clone(),
            request,
            aggregate_token_budget.clone(),
            self.loop_tools.len(),
            self.account_hops,
        )?;
        let claude_context = crate::claude_request_context::for_request(
            &crate::store::config_client::ConfigAccess::local_managed_agent(
                self.node.clone(),
                identity::Did::new(request.agent_did.clone())?,
                self.agent_config.agent_identity.as_ref(),
            )?,
            &self.agent_config,
            request,
            gents_claude::origin::Kind::Turn,
        )
        .await?;
        crate::claude_request_context::apply(&mut loop_config, claude_context.as_ref())?;
        loop_config.deadline = request_deadline;
        loop_config.context_calibration = context_calibration.clone();
        loop_config.output_obligation_gate =
            output_obligation_gate.map(|gate| Arc::new(gate) as Arc<dyn OutputObligationCheck>);
        let turn_compactor = self.compactor.clone();
        let provider_profile = loop_config.provider_input_counter.profile();
        let turn_context_window = self.agent_config.context_window;
        let max_turns_provenance = self.agent_config.max_turns_provenance;
        let turn_compaction_options = self.compaction_options_for_request(
            request_deadline,
            aggregate_token_budget,
            effective_seed,
            context_calibration.clone(),
            claude_context
                .as_ref()
                .map(crate::claude_request_context::compaction_params)
                .transpose()?,
        );
        let turn_node = self.node.clone();
        let turn_agent_did = self.agent_config.agent_did.clone();
        let turn_agent_signer = self
            .agent_config
            .agent_identity
            .managed_agent_signer()
            .context("provider reduction requires a managed Agent signer")?;
        let turn_request = request.clone();
        let turn_request_commit_cid = lifecycle
            .request_commit_cid()
            .context("claimed request has no exact commit CID for per-turn reduction")?
            .to_string();
        loop_config.turn_compactor = Some(turn_compactor_callback(TurnCompaction {
            compactor: turn_compactor,
            options: turn_compaction_options,
            node: turn_node,
            agent_did: turn_agent_did,
            signer: turn_agent_signer,
            request: turn_request,
            request_commit_cid: turn_request_commit_cid,
            provider_profile,
            turn_context_window,
        }));
        loop_config.context_message = request_context_message.clone();
        loop_config.resume_from_history = resume_from_history;
        loop_config.folded_prompts = folded_prompts.clone();
        let (loop_history, loop_prompt) = self
            .request_frontier(preparation, &mut loop_config, &mut replay)
            .await?;
        loop_config.replay = replay;
        Ok(PreparedInference {
            hook,
            persistence_hook,
            model,
            loop_config,
            loop_history,
            loop_prompt,
            max_turns_provenance,
        })
    }

    async fn session_hook(
        &self,
        preparation: InferencePreparation<'_>,
    ) -> Result<(DefraSessionHook, Option<OutputObligationGate>)> {
        let InferencePreparation {
            request,
            lifecycle,
            request_deadline,
            inference_observation,
            resume_from_history,
            ..
        } = preparation;
        let output_obligation_gate =
            crate::runtime::agent::output_obligation::OutputObligationGate::for_request(
                self.node.clone(),
                request,
                self.output_obligations.as_ref(),
            )
            .await?;
        let hook = DefraSessionHook::resume_with_actor_policy(
            self.node.clone(),
            &request.session_id,
            self.agent_config.node_did(),
            request.requester_did.as_deref(),
            ::identity::Did::new(request.agent_did.clone())?,
            self.hook_failure_policy,
        )
        .await?
        .with_managed_identity(self.agent_config.agent_identity.as_ref())?
        .with_inference_observation(inference_observation.clone())
        .with_background_tool_registry(self.background_tool_registry.clone())
        .with_background_execution_registry(self.background_execution_registry.clone())
        .with_operator_tool_root(self.operator_tool_root.clone())
        .with_remote_tools(self.remote_tools.clone())
        .with_job_tools(self.job_tools.clone())
        .with_goal_tool_authority(
            self.agent_config.tools.goal_tools_requested(),
            self.agent_config.tools.goal_creation_requested(),
        )
        .with_output_obligation_gate(output_obligation_gate.clone());
        // Lean `CurrentInput.answersSelection`: a resumed retry
        // publishes no authored input, so steered messages stay
        // queued for their own turn instead of folding into it.
        let hook = if resume_from_history {
            hook
        } else {
            hook.with_steering_inputs(
                request.clone(),
                self.request_admission.clone(),
                self.agent_config.agent_identity.clone(),
                lifecycle.natural_finish_allowed(),
            )
        };
        let hook = match &self.job_driver {
            Some(driver) => hook.with_job_driver(driver.clone()),
            None => hook,
        };
        hook.set_active_request_binding(
            Some(request.request_id.clone()),
            Some(request.doc_id.clone()),
            request.requester_did.clone(),
        )
        .await;
        hook.set_request_deadline_at(request_deadline).await;
        Ok((hook, output_obligation_gate))
    }

    async fn request_frontier(
        &self,
        preparation: InferencePreparation<'_>,
        loop_config: &mut LoopConfig,
        replay: &mut LoopReplayInput,
    ) -> Result<(Vec<TaggedMessage>, TaggedMessage)> {
        let InferencePreparation {
            request,
            request_context_message,
            resume_from_history,
            consumed_prompts,
            folded_prompts,
            history,
            wake_notifications,
            ..
        } = preparation;
        let authored_prompt = request.input.authored_message(&request.content)?;
        let authored_input = |folded: Vec<crate::runtime::agent::loop_stream::FoldedPrompt>| {
            crate::runtime::agent::loop_stream::AuthoredInput {
                context: request_context_message.clone(),
                prompt: authored_prompt.clone(),
                folded,
            }
        };
        loop_config.authored_input = Some(authored_input(folded_prompts.clone()));
        let reduction_access = crate::store::config_client::ConfigAccess::local_managed_agent(
            self.node.clone(),
            identity::Did::new(request.agent_did.clone())?,
            self.agent_config.agent_identity.as_ref(),
        )?;
        let restored = crate::session::provider_context_reduction::load_unconsumed_for_request(
            &reduction_access,
            &request.doc_id,
        )
        .await?;
        let (loop_history, loop_prompt) = if let Some((row, lineage_keys)) = restored {
            let mut messages = row.checkpoint_tagged_messages()?;
            let prompt = messages
                .pop()
                .context("durable provider-context checkpoint has no current prompt")?;
            anyhow::ensure!(
                row.node_did == request.node_did
                    && row.requester_did == request.requester_did
                    && row.session_id == request.session_id
                    && row.request_id == request.request_id
                    && row.request_doc_id == request.doc_id,
                "durable provider-context checkpoint crossed its current request scope"
            );
            let boundary = row.source_boundary()?;
            let replay_scope = crate::session::CanonicalReplayScope {
                expected_scope_kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                node_did: &request.node_did,
                requester_did: request.requester_did.as_deref(),
                session_id: &request.session_id,
                request_id: &request.request_id,
                request_doc_id: &request.doc_id,
                request_commit_cid: &row.request_commit_cid,
            };
            replay.required = row.replay_associations()?.required;
            crate::session::validate_canonical_replay_boundary(
                &reduction_access,
                replay_scope,
                &boundary,
            )
            .await?;
            loop_config.context_message = None;
            // The checkpoint's provider view already holds every
            // consumed message; a selection made at this claim
            // waits for the next turn.
            loop_config.folded_prompts = Vec::new();
            loop_config.authored_input = Some(authored_input(consumed_prompts.clone()));
            loop_config.active_reduction_keys = row.active_reduction_keys();
            loop_config.reduction_chain_keys = lineage_keys;
            loop_config.initial_turn_index = usize::try_from(row.turn_index)
                .context("durable provider-context checkpoint has invalid turn index")?;
            tracing::info!(
                request_id = %request.request_id,
                reduction_key = %row.reduction_key,
                reduction_index = row.reduction_index,
                "restored unconsumed durable provider-context checkpoint"
            );
            (messages, prompt)
        } else {
            loop_config.reduction_chain_keys =
                crate::session::provider_context_reduction::load_for_request(
                    &reduction_access,
                    &request.doc_id,
                )
                .await?
                .into_iter()
                .map(|row| row.reduction_key)
                .collect();
            let entry = super::request_entry(
                history.to_vec(),
                &authored_prompt,
                resume_from_history,
                loop_config.context_message.as_ref(),
                wake_notifications,
            )?;
            if resume_from_history {
                loop_config.context_message = None;
            }
            entry
        };
        Ok((loop_history, loop_prompt))
    }
}

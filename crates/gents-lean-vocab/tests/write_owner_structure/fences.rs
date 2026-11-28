/// Production transaction units that read by filter, list, count or order and
/// then write, by `path::function` (`::transact_*` for a callback), with how
/// many such units the name covers and the fence their decision rests on, in
/// the vocabulary of Lean `TransactionFence.DeclaredFence`. Only shrinks.
pub(crate) const FENCES: &[(&str, usize, &str)] = &[
    (
        "crates/gents-automation/src/goal/claimed_publication.rs::stage_claimed_continuation",
        1,
        "CommonWrite(Goal): the continuation is a filtered update of the Goal on its status and continuation_sequence, which every other continuation of the Goal also writes",
    ),
    (
        "crates/gents-automation/src/goal/controller.rs::update_goal_fields_if_status_observed::transact_write",
        1,
        "CommonWrite(Goal): the change is a filtered update of the Goal on the status it observed",
    ),
    (
        "crates/gents-automation/src/goal/creation.rs::finish_goal_relationships::transact_signed_conflict_rebuild",
        1,
        "Idempotent(each repair is a filtered update of one unlinked row on parent_linked; a row the scan misses is linked by its creator's own repair)",
    ),
    (
        "crates/gents-automation/src/goal/first_request_binding.rs::bind_goal_first_request_in_txn",
        1,
        "CommonWrite(Goal): the binding is a filtered update of the Goal on an unset assignment_root_request_doc_id",
    ),
    (
        "crates/gents-automation/src/goal/observation.rs::project_session_goal::transact_write",
        1,
        "UniqueIndex(GoalObservation(session_doc_id)): a session's observation is created only under its unique session document id, otherwise updated by document id",
    ),
    (
        "crates/gents-automation/src/goal/session_goal_control.rs::process_command_with_clock::transact_idempotent",
        1,
        "UniqueIndex(AgentSessionGoalControl(command_id)): no twin of the command can exist, so the twin scan only reads; the receipt is unique on command_doc_id",
    ),
    (
        "crates/gents-automation/src/operations/goal.rs::apply_set_in_txn",
        1,
        "CommonWrite(Goal): the change is a filtered update of the Goal on its status and continuation_sequence",
    ),
    (
        "crates/gents-automation/src/triggers/engine/durable.rs::stage_fire_receipt",
        1,
        "UniqueIndex(TriggerFire(owner_did, trigger_id, fire_sequence)): each receipt takes the sequence one past the receipts its admission counted",
    ),
    (
        "crates/gents-automation/src/triggers/engine/durable.rs::supersede_trigger_requests_in_txn",
        1,
        "SingleWriter(the trigger engine's per-trigger dispatch lock; supersession runs only for a LatestOnly fire, which the engine dispatches under its Trigger's lock): each superseded request is a filtered update on its lifecycle state",
    ),
    (
        "crates/gents-documents/src/desired_state/apply.rs::apply_desired_state_plan_with_semantics",
        1,
        "UniqueIndex(configuration documents(node_did, id)): every document the plan creates is created under its collection's unique key and every existing one is updated by document id",
    ),
    (
        "crates/gents-eval/src/optimization/job.rs::append_in_txn",
        1,
        "CommonWrite(OptimizationJob): the append is a filtered update on the job's journal_len, which every append writes",
    ),
    (
        "crates/gents-inference/src/backends/registry.rs::record_model_catalog_for_document_in_txn",
        1,
        "PointRead(InferenceBackendObservation): the filter is the unique (node_did, backend_id), written by document id",
    ),
    (
        "crates/gents-inference/src/backends/registry.rs::write_probe_observation::transact_write",
        1,
        "PointRead(InferenceBackendObservation): the filter is the unique (node_did, backend_id), written by document id",
    ),
    (
        "crates/gents-p2p/src/p2p_reconcile/engine/store.rs::persist_applied::transact_write",
        2,
        "UniqueIndex(PeerPairingApplied(node_did, peer_id)): an applied pairing is created only under its unique key; deletes name document ids or that key",
    ),
    (
        "crates/gents-p2p/src/p2p_reconcile/engine/store/materialize.rs::materialize_enrollment_desired_in_txn",
        1,
        "UniqueIndex(PeerPairingDesired(peer_id)): a route for a peer is created only under the unique peer_id, otherwise updated by document id",
    ),
    (
        "crates/gents-session/src/request/lifecycle/backend_wait.rs::park_in_txn",
        1,
        "SingleWriter(the request's execution lease holder starts its foreground tool calls and parks the request; the park is a filtered update on the request's lifecycle state)",
    ),
    (
        "crates/gents-session/src/request/lifecycle/canonical_recovery.rs::recover_expired_generation_with_access_and_facts::transact_idempotent",
        1,
        "CommonWrite(AgentRequest): recovery writes the request on the execution generation it observed, which every claim and terminal of the request writes",
    ),
    (
        "crates/gents-session/src/request/lifecycle/claim.rs::claim_request_with_projection::transact_write",
        1,
        "CommonWrite(AgentSession): every claim writes its session's observation, so two claims of one session's queue abort each other",
    ),
    (
        "crates/gents-session/src/request/lifecycle/client.rs::stage_client_request_checked_in_txn",
        1,
        "CommonWrite(AgentRequest): a signed create's document id is derived from its signed content, so two submissions of one signed request create one document; a send into a published session reads the session's binding before the transaction, under ImmutableBinding(AgentSession(requester_did, agent_did, relationships_pending)) (`bound_published_session`)",
    ),
    (
        "crates/gents-session/src/request/lifecycle/queue/atomic_inputs.rs::background_completion_transaction_attempt",
        1,
        "CommonWrite(AgentSession): every wake publication and redrive of a session writes the session document (touch_wake_session_in_txn)",
    ),
    (
        "crates/gents-session/src/request/lifecycle/queue/draining.rs::drain_pending_session_requests_where_in_txn",
        1,
        "Idempotent(a pending request the scan misses is drained by its own claim, which reads the session's stop; each drained request is a filtered update on its pending state)",
    ),
    (
        "crates/gents-session/src/request/lifecycle/queue/folding.rs::consume_folded_in_txn",
        1,
        "CommonWrite(AgentRequest): each folded request is a filtered update on its pending state",
    ),
    (
        "crates/gents-session/src/request/lifecycle/terminal_tools.rs::account_tools_in_txn",
        1,
        "SingleWriter(the request's execution lease holder creates its tool calls and accounts them at its terminal; each is a filtered update by document id)",
    ),
    (
        "crates/gents-session/src/request/streaming/canonical/publish.rs::publish_provider_turn_with_time::transact_idempotent",
        1,
        "UniqueIndex(AgentToolCall(tool_call_key)): the turn's tool calls are created under their unique keys, and a replay finds the published turn",
    ),
    (
        "crates/gents-session/src/session/compaction_entries.rs::save_compaction_entry_with_access::transact_idempotent",
        1,
        "UniqueIndex(CompactionEntry(compaction_key)): the entry is created under its unique key; the ordered message read only bounds the range it summarizes",
    ),
    (
        "crates/gents-session/src/session/fork.rs::copy_fork_inventory",
        1,
        "Idempotent(the fork copies the inventory it read as a snapshot of the source session; a row the source gains later belongs to the source only)",
    ),
    (
        "crates/gents-session/src/session/sessions.rs::ensure_session_with_pending_in_txn",
        1,
        "UniqueIndex(AgentSession(node_did, session_id)): the session is created only under its unique key",
    ),
    (
        "crates/gents-session/src/workspace/runtime.rs::claim_workspace_cleanup::transact_write",
        1,
        "CommonWrite(IsolatedWorkspace): the cleanup writes the workspace, which every exclusive admission writes",
    ),
    (
        "crates/gents-session/src/workspace/runtime.rs::materialize_workspace_binding::transact_write",
        1,
        "CommonWrite(IsolatedWorkspace): every ReadWrite or Integrate admission writes binding_revision (concurrent_exclusive_admissions_of_one_workspace_bind_one_writer)",
    ),
    (
        "crates/gents-store/src/client_journal/records/goal.rs::reserve::transact_local_idempotent",
        1,
        "UniqueIndex(PendingChatSubmission(request_id)): the reservation is created only under its unique request id",
    ),
    (
        "crates/gents-toolcalls/src/call/control/client.rs::claim_inner::transact_write",
        1,
        "CommonWrite(AgentToolControl): every claim of an intent updates the intent's state",
    ),
    (
        "crates/gents-toolcalls/src/call/lifecycle/delivery/background_receipt.rs::publish_background_receipt_in_txn",
        1,
        "CommonWrite(AgentToolCall): the receipt is a filtered update of its running tool call, which every receipt and terminal of the call writes",
    ),
    (
        "crates/gents-toolcalls/src/call/lifecycle/delivery/spawned.rs::admit_spawned_background_child_with_time::transact_idempotent",
        1,
        "UniqueIndex(AgentToolCall(tool_call_key)): the child is created only under its stable key",
    ),
    (
        "crates/gents-toolcalls/src/call/lifecycle/delivery/terminal_effects.rs::retain_file_changes",
        1,
        "CommonWrite(AgentToolCall): file changes are written in the call's terminal transaction, which writes the call",
    ),
    (
        "crates/gents-toolcalls/src/call/lifecycle/plugin_effect.rs::admit_plugin_effect_with_mode::transact_idempotent",
        1,
        "UniqueIndex(AgentToolCall(tool_call_key)): the effect's call is created only under its stable key",
    ),
    (
        "crates/gents-tools/src/mcp/health.rs::upsert_persisted_health_state::transact_write",
        1,
        "SingleWriter(the runtime's MCP health checker is the one writer of a service's health state)",
    ),
    (
        "crates/gents-tools/src/memory/experience_store.rs::ensure_problem_in_txn",
        1,
        "UniqueIndex(AgentMemoryProblem(problem_key)): the problem is created only under its unique key, and a twin returns the committed row",
    ),
    (
        "crates/gents-tools/src/memory/experience_write.rs::stage_attempt_in_txn",
        1,
        "UniqueIndex(AgentMemoryAttempt(attempt_key)): the ordinal is read inside the transaction and the attempt is created only under its unique key",
    ),
    (
        "crates/gents-tools/src/memory/experience_write.rs::outcome::transact_write",
        1,
        "UniqueIndex(AgentMemoryOutcome(outcome_key)): the ordinal is read inside the transaction and the outcome is created only under its unique key",
    ),
    (
        "crates/gents-tools/src/memory/experience_write.rs::merge::transact_write",
        1,
        "SingleWriter(AgentMemoryAliasGuard(agent_did)): every admitted merge writes the agent's one guard row, so a concurrent merge conflicts and rebuilds its admission read",
    ),
    (
        "crates/gents/src/runtime/agent/session_input_edit.rs::process_command_with_now::transact_idempotent",
        1,
        "UniqueIndex(AgentSessionInputEdit(command_id)): no twin of the command can exist, so the twin scan only reads",
    ),
];

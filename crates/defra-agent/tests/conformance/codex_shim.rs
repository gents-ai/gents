use super::*;

pub(super) fn generated_codex_shim_projection_cases_pin_adapter_mapping() {
    let cases = lean_codex_shim_projection_cases();
    assert_eq!(cases.len(), 12);

    for case in cases {
        assert_eq!(
            case.terminal, case.effectively_terminal,
            "{} should satisfy CodexShim.codex_turn_terminates_precisely",
            case.witness
        );
        assert!(
            case.lean_theorems
                .contains(&"CodexShim.codex_turn_terminates_precisely".to_string()),
            "{} should cite terminal coherence",
            case.witness
        );
        if case.local_interrupt_acked {
            assert!(
                case.interruptible_request_state,
                "{} must only acknowledge local interrupts for interruptible states",
                case.witness
            );
            assert!(
                case.lean_theorems
                    .contains(&"CodexShim.local_interrupt_requires_interruptible".to_string()),
                "{} should cite local interrupt eligibility",
                case.witness
            );
            assert!(
                case.lean_theorems
                    .contains(&"CodexShim.local_interrupt_shortcut_sound".to_string()),
                "{} should cite local interrupt soundness",
                case.witness
            );
        }
    }

    let names = cases
        .iter()
        .map(|case| case.witness.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "codex_shim.projection.pending_no_response",
            "codex_shim.projection.claimed_no_response",
            "codex_shim.projection.processing_streaming_response",
            "codex_shim.projection.nonterminal_complete_response",
            "codex_shim.projection.nonterminal_error_response",
            "codex_shim.projection.completed_request",
            "codex_shim.projection.failed_request",
            "codex_shim.projection.dead_request",
            "codex_shim.projection.superseded_request",
            "codex_shim.projection.interrupted_request",
            "codex_shim.projection.local_interrupt_preempts_core_state",
            "codex_shim.projection.local_interrupt_input_required",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    let pending = lean_codex_shim_projection_case("codex_shim.projection.pending_no_response");
    assert_eq!(pending.request_state, "pending");
    assert_eq!(pending.response_status, None);
    assert!(!pending.local_interrupt_acked);
    assert_eq!(pending.projected_phase, "inProgress");
    assert!(!pending.terminal);
    assert!(!pending.effectively_terminal);
    assert!(!pending.interruptible_request_state);
    assert_eq!(
        pending.lean_theorems,
        vec![
            "CodexShim.project_pending_is_in_progress".to_string(),
            "CodexShim.nonterminal_without_response_projects_in_progress".to_string(),
            "CodexShim.request_transition_projection_monotonic".to_string(),
            "CodexShim.codex_turn_terminates_precisely".to_string(),
        ]
    );

    let completed = lean_codex_shim_projection_case("codex_shim.projection.completed_request");
    assert_eq!(completed.request_state, "completed");
    assert_eq!(completed.response_status.as_deref(), Some("error"));
    assert!(!completed.local_interrupt_acked);
    assert_eq!(completed.projected_phase, "completed");
    assert!(completed.terminal);
    assert!(completed.effectively_terminal);
    assert!(!completed.interruptible_request_state);
    assert!(completed
        .lean_theorems
        .contains(&"CodexShim.terminal_request_overrides_response".to_string()));

    let local_interrupt = lean_codex_shim_projection_case(
        "codex_shim.projection.local_interrupt_preempts_core_state",
    );
    assert_eq!(local_interrupt.request_state, "processing");
    assert_eq!(
        local_interrupt.response_status.as_deref(),
        Some("streaming")
    );
    assert!(local_interrupt.local_interrupt_acked);
    assert_eq!(local_interrupt.projected_phase, "interrupted");
    assert!(local_interrupt.terminal);
    assert!(local_interrupt.effectively_terminal);
    assert!(local_interrupt.interruptible_request_state);
    assert_eq!(
        local_interrupt.lean_theorems,
        vec![
            "CodexShim.local_interrupt_projects_interrupted".to_string(),
            "CodexShim.local_interrupt_never_projects_in_progress".to_string(),
            "CodexShim.codex_turn_terminates_precisely".to_string(),
            "CodexShim.local_interrupt_requires_interruptible".to_string(),
            "CodexShim.local_interrupt_shortcut_sound".to_string(),
        ]
    );

    let input_required =
        lean_codex_shim_projection_case("codex_shim.projection.local_interrupt_input_required");
    assert_eq!(input_required.request_state, "inputRequired");
    assert!(input_required.local_interrupt_acked);
    assert!(input_required.interruptible_request_state);
    assert_eq!(input_required.projected_phase, "interrupted");

    let lifecycle_cases = lean_codex_shim_turn_lifecycle_cases();
    assert_eq!(lifecycle_cases.len(), 4);
    let lifecycle_names = lifecycle_cases
        .iter()
        .map(|case| case.witness.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        lifecycle_names,
        [
            "codex_shim.turn_lifecycle.start",
            "codex_shim.turn_lifecycle.complete",
            "codex_shim.turn_lifecycle.fail",
            "codex_shim.turn_lifecycle.interrupt",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );
    for case in lifecycle_cases {
        assert!(
            case.monotonic,
            "{} should be marked monotonic by CodexShim.turn_lifecycle_never_regresses",
            case.witness
        );
        assert!(
            case.post_lex_ord >= case.pre_lex_ord,
            "{} must not regress in TurnPhase.lexOrd",
            case.witness
        );
        assert!(
            case.lean_theorems
                .contains(&"CodexShim.turn_lifecycle_never_regresses".to_string()),
            "{} should cite turn lifecycle monotonicity",
            case.witness
        );
    }
    let interrupt = lifecycle_cases
        .iter()
        .find(|case| case.witness == "codex_shim.turn_lifecycle.interrupt")
        .expect("interrupt lifecycle witness");
    assert_eq!(interrupt.action, "interrupt");
    assert_eq!(interrupt.pre_phase, "inProgress");
    assert_eq!(interrupt.post_phase, "interrupted");
    assert!(interrupt
        .lean_theorems
        .contains(&"CodexShim.interrupt_step_is_terminal".to_string()));

    let tool_cases = lean_codex_shim_subagent_tool_cases();
    assert_eq!(tool_cases.len(), 9);
    for case in tool_cases {
        let expected = match case.witness.as_str() {
            "codex_shim.subagent_tool.spawn" => ("collabAgentToolCall", Some("spawnAgent")),
            "codex_shim.subagent_tool.wait" => ("collabAgentToolCall", Some("wait")),
            "codex_shim.subagent_tool.steer" => ("collabAgentToolCall", Some("sendInput")),
            "codex_shim.subagent_tool.cancel" => ("collabAgentToolCall", Some("closeAgent")),
            "codex_shim.subagent_tool.list" | "codex_shim.subagent_tool.read" => {
                ("mcpToolCall", None)
            }
            "codex_shim.subagent_tool.unresolved_open" => ("deferred", None),
            "codex_shim.subagent_tool.unresolved_settling" => ("deferred", None),
            "codex_shim.subagent_tool.unresolved_settled" => ("mcpToolCall", None),
            other => panic!("unmodeled subagent tool witness {other:?}"),
        };
        assert_eq!(case.projected_item_kind, expected.0, "{}", case.witness);
        assert_eq!(case.collab_tool.as_deref(), expected.1, "{}", case.witness);
        if case.projected_item_kind == "collabAgentToolCall" {
            assert!(case.reciprocal_link, "{}", case.witness);
        }
        if case.witness == "codex_shim.subagent_tool.spawn" {
            assert_eq!(case.runtime_tool_status.as_deref(), Some("inProgress"));
            assert_eq!(case.projected_collab_status.as_deref(), Some("completed"));
            assert!(case.lean_theorems.contains(
                &"CodexShim.linked_spawn_operation_completes_while_child_runs".to_string()
            ));
        }
        if case.witness == "codex_shim.subagent_tool.unresolved_settling" {
            assert!(case.projection_settled, "{}", case.witness);
            assert!(!case.link_settle_expired, "{}", case.witness);
        }
        if case.witness == "codex_shim.subagent_tool.unresolved_settled" {
            assert!(case.projection_settled, "{}", case.witness);
            assert!(case.link_settle_expired, "{}", case.witness);
        }
    }

    let status_cases = lean_codex_shim_subagent_status_cases();
    assert_eq!(status_cases.len(), 9);
    for case in status_cases {
        let expected = match case.request_state.as_str() {
            "pending" => ("pendingInit", false),
            "claimed" | "processing" | "inputRequired" => ("running", false),
            "completed" => ("completed", true),
            "failed" | "dead" => ("errored", true),
            "superseded" | "interrupted" => ("interrupted", true),
            other => panic!("{}: unmodeled child request state {other:?}", case.witness),
        };
        assert_eq!(case.projected_agent_status, expected.0, "{}", case.witness);
        assert_eq!(case.terminal, expected.1, "{}", case.witness);
        assert!(case
            .lean_theorems
            .contains(&"CodexShim.subagent_status_terminal_precisely".to_string()));
    }

    let visibility_cases = lean_codex_shim_subagent_visibility_cases();
    assert_eq!(visibility_cases.len(), 4);
    for case in visibility_cases {
        let expected = if !case.authorized {
            "hidden"
        } else if case.loaded {
            "live"
        } else {
            "snapshot"
        };
        assert_eq!(case.projection_mode, expected, "{}", case.witness);
    }

    let metadata_cases = lean_codex_shim_subagent_metadata_cases();
    assert_eq!(metadata_cases.len(), 2);
    for case in metadata_cases {
        assert_eq!(case.projected_model, case.runtime_model, "{}", case.witness);
        assert_eq!(
            case.projected_reasoning_effort, case.runtime_reasoning_effort,
            "{}: adapter must not invent reasoning effort",
            case.witness
        );
        assert!(case
            .lean_theorems
            .contains(&"CodexShim.collab_model_is_runtime_model".to_string()));
    }

    let listing_cases = lean_codex_shim_subagent_listing_cases();
    assert_eq!(listing_cases.len(), 5);
    for case in listing_cases {
        let source_matches = matches!(
            case.source_kind.as_str(),
            "subAgent" | "subAgentThreadSpawn"
        );
        assert_eq!(
            case.listed,
            case.authorized && source_matches,
            "{}: listing must require authorization and a spawned-subagent source filter",
            case.witness
        );
    }

    let shape_cases = lean_codex_shim_subagent_thread_shape_cases();
    assert_eq!(shape_cases.len(), 1);
    let shape = &shape_cases[0];
    assert_eq!(
        shape.native_source_parent.as_deref(),
        Some(shape.parent_thread_id.as_str())
    );
    assert_eq!(shape.legacy_top_level_parent, None);
    assert_eq!(shape.replay_stages, ["user", "compaction", "modelItems"]);

    let reasoning_cases = lean_codex_shim_reasoning_projection_cases();
    assert_eq!(reasoning_cases.len(), 9);
    for case in reasoning_cases {
        assert_eq!(
            case.projected_delta,
            case.live_delta
                .as_ref()
                .filter(|text| !text.is_empty())
                .cloned()
                .or_else(|| {
                    (case.terminal
                        && !case.item_open
                        && !case.item_completed
                        && !case.cursor_primed)
                        .then(|| case.durable_text.clone())
                        .flatten()
                        .filter(|text| !text.is_empty())
                }),
            "{}: live delta or first durable observation must drive raw reasoning text",
            case.witness
        );
        assert_eq!(
            case.completed_text,
            (case.terminal && !case.item_completed)
                .then(|| {
                    case.durable_text
                        .clone()
                        .filter(|text| !text.is_empty())
                        .or_else(|| case.streamed_text.clone())
                })
                .flatten()
                .filter(|text| !text.is_empty()),
            "{}: completed reasoning must prefer durable text and retain the streamed fallback",
            case.witness
        );
        assert!(
            !case
                .projected_events
                .iter()
                .any(|event| event == "summaryTextDelta"),
            "{}: raw DEFRA reasoning must not be promoted to a summary",
            case.witness
        );
    }
    let first = reasoning_cases
        .iter()
        .find(|case| case.witness == "codex_shim.reasoning.first_live")
        .expect("first live reasoning witness");
    assert_eq!(first.projected_events, ["started", "rawTextDelta"]);
    let resumed = reasoning_cases
        .iter()
        .find(|case| case.witness == "codex_shim.reasoning.resumed_unchanged")
        .expect("resumed reasoning witness");
    assert!(resumed.projected_events.is_empty());
    let terminal_first = reasoning_cases
        .iter()
        .find(|case| case.witness == "codex_shim.reasoning.terminal_first_observation")
        .expect("terminal first reasoning witness");
    assert_eq!(
        terminal_first.projected_events,
        ["started", "rawTextDelta", "completed"]
    );
    let durable_suffix = reasoning_cases
        .iter()
        .find(|case| case.witness == "codex_shim.reasoning.terminal_durable_suffix")
        .expect("terminal durable suffix witness");
    assert_eq!(
        durable_suffix.projected_events,
        ["rawTextDelta", "completed"]
    );
    assert_eq!(
        durable_suffix.projected_delta.as_deref(),
        Some("; durable suffix")
    );
    let reset_before_terminal = reasoning_cases
        .iter()
        .find(|case| case.witness == "codex_shim.reasoning.reset_before_terminal")
        .expect("reset-before-terminal reasoning witness");
    assert!(reset_before_terminal.projected_events.is_empty());
    assert_eq!(reset_before_terminal.projected_delta, None);
    assert_eq!(reset_before_terminal.completed_text, None);

    let thread_status_cases = lean_codex_shim_thread_status_cases();
    assert_eq!(thread_status_cases.len(), 11);
    for case in thread_status_cases {
        let expected = match case.request_state.as_deref() {
            Some("pending" | "claimed" | "processing" | "inputRequired") => "active",
            Some("failed" | "dead") => "systemError",
            Some("completed" | "superseded" | "interrupted") => "idle",
            None if case.conversation_status == "error" => "systemError",
            None => "idle",
            Some(other) => panic!("{}: unknown request state {other}", case.witness),
        };
        assert_eq!(case.projected_status, expected, "{}", case.witness);
    }

    let behavior_cases = lean_codex_shim_behavior_selection_cases();
    assert_eq!(behavior_cases.len(), 5);
    for case in behavior_cases {
        let expected = case
            .thread_behavior_id
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(&case.root_behavior_id);
        assert_eq!(case.projected_behavior_id, expected, "{}", case.witness);
        let projected_model = case
            .resolved_child_model
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                case.projected_child_model
                    .as_deref()
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or(&case.root_model);
        assert_eq!(case.projected_model, projected_model, "{}", case.witness);
    }

    let tool_metadata_cases = lean_codex_shim_tool_metadata_cases();
    assert_eq!(tool_metadata_cases.len(), 11);
    for case in tool_metadata_cases {
        let nonempty = |value: &Option<String>| {
            value
                .as_deref()
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        };
        assert_eq!(
            case.projected_server,
            nonempty(&case.selected_server).unwrap_or_else(|| case.fallback_server.clone()),
            "{}: server identity",
            case.witness
        );
        assert_eq!(
            case.projected_tool,
            nonempty(&case.selected_tool).unwrap_or_else(|| case.fallback_tool.clone()),
            "{}: tool identity",
            case.witness
        );
        assert_eq!(
            case.projected_failure,
            nonempty(&case.denial_reason)
                .or_else(|| nonempty(&case.cancel_cause))
                .or_else(|| nonempty(&case.result_fallback))
                .or_else(|| nonempty(&case.failure_class)),
            "{}: failure diagnostic",
            case.witness
        );
        assert_eq!(
            case.projected_duration_ms,
            case.latency_ms.or_else(|| {
                case.started_at_ms
                    .zip(case.completed_at_ms)
                    .map(|(started, completed)| completed.saturating_sub(started))
            }),
            "{}: duration",
            case.witness
        );
        assert_eq!(
            case.projected_event_at_ms,
            case.persisted_event_at_ms.unwrap_or(case.observed_at_ms),
            "{}: event timestamp",
            case.witness
        );
    }

    let context_cases = lean_codex_shim_context_usage_cases();
    assert_eq!(context_cases.len(), 2);
    for case in context_cases {
        assert_eq!(
            case.total_tokens,
            case.cumulative_input + case.cumulative_output,
            "{}: cumulative accounting drifted",
            case.witness
        );
        assert_eq!(
            case.current_context_tokens,
            case.latest_prompt + case.latest_completion,
            "{}: current context must come from the latest inference call",
            case.witness
        );
        assert_eq!(
            case.remaining_tokens,
            case.model_window
                .saturating_sub(case.current_context_tokens),
            "{}: remaining context must saturate at zero",
            case.witness
        );
        assert!(case
            .lean_theorems
            .contains(&"CodexShim.current_context_uses_latest_call".to_string()));
    }

    let compaction_cases = lean_codex_shim_compaction_projection_cases();
    assert_eq!(compaction_cases.len(), 8);
    for case in compaction_cases {
        assert_eq!(
            case.claims_compacted,
            case.projected_events
                .iter()
                .any(|event| event == "completed"),
            "{}: only a completed item may claim context was compacted",
            case.witness
        );
        match (
            case.previous_call_state.as_deref(),
            case.call_state.as_str(),
        ) {
            (None, "queued" | "running") => {
                assert_eq!(case.projected_events, ["started"])
            }
            (None, "completed") => {
                assert_eq!(case.projected_events, ["started", "completed"])
            }
            (Some("running"), "completed") => {
                assert_eq!(case.projected_events, ["completed"])
            }
            (Some("running"), "failed" | "cancelled") => {
                assert!(case.projected_events.is_empty())
            }
            (None, "failed" | "cancelled") => assert!(case.projected_events.is_empty()),
            other => panic!(
                "{}: unmodeled compaction projection {other:?}",
                case.witness
            ),
        }
    }
}

/// Fence for the runnable-gated shim binding (#699).
///
/// Drives the real `ShimBinding` through every vector the Lean model emits. The
/// shim disabled itself at boot on an empty store and never rebound when
/// `config apply` later made the behavior runnable; these vectors pin that a
/// published generation — not a process restart — is what binds it.
pub(super) fn generated_codex_shim_binding_cases_pin_runnable_gated_binding() {
    use defra_agent::codex_shim_binding::{ShimBinding, ShimBindingState, ShimUnboundReason};

    let cases = lean_codex_shim_binding_cases();
    assert_eq!(
        cases.len(),
        5,
        "the Lean binding contract must stay fully consumed"
    );

    const BOUND_BEHAVIOR: &str = "default";

    let reason_of = |state: ShimBindingState| match state {
        ShimBindingState::Bound => None,
        ShimBindingState::Unbound(ShimUnboundReason::DependencyMissing) => {
            Some("dependencyMissing")
        }
        ShimBindingState::Unbound(ShimUnboundReason::HostResource) => Some("hostResource"),
    };

    for case in cases {
        assert!(
            !case.requires_restart,
            "{}: convergence must follow from a published generation, never a restart",
            case.witness
        );

        let mut shim = match (case.pre_state.as_str(), case.unbound_reason.as_deref()) {
            ("bound", None) => ShimBinding::bound(BOUND_BEHAVIOR),
            ("unbound", Some("dependencyMissing")) => {
                ShimBinding::unbound(BOUND_BEHAVIOR, ShimUnboundReason::DependencyMissing)
            }
            ("unbound", Some("hostResource")) => {
                ShimBinding::unbound(BOUND_BEHAVIOR, ShimUnboundReason::HostResource)
            }
            other => panic!("{}: unmodeled pre-state {other:?}", case.witness),
        };

        // The published generation either carries the bound behavior as runnable
        // or it does not.
        let runnable: Vec<&str> = if case.bound_behavior_runnable {
            vec!["other", BOUND_BEHAVIOR]
        } else {
            vec!["other"]
        };

        // The listener is only ever acquired when the generation authorizes it.
        let mut listen_attempts = 0usize;
        let host_can_listen = case.host_can_listen;
        let state = shim.observe_publish(runnable.iter().copied(), || {
            listen_attempts += 1;
            host_can_listen
        });

        let observed = if shim.is_bound() { "bound" } else { "unbound" };
        assert_eq!(
            observed, case.post_state,
            "{}: observing a published generation must land in the modeled state",
            case.witness
        );
        assert_eq!(
            reason_of(state),
            case.post_unbound_reason.as_deref(),
            "{}: the unbound class decides whether a later generation may revive \
             the shim; it must match the model",
            case.witness
        );

        // The listener is acquired only when the dependency was actually
        // supplied — never speculatively, and never for a host-resource fixpoint.
        let expected_attempts = usize::from(
            case.bound_behavior_runnable
                && case.pre_state == "unbound"
                && case.unbound_reason.as_deref() == Some("dependencyMissing"),
        );
        assert_eq!(
            listen_attempts, expected_attempts,
            "{}: the host must attempt the listen exactly when the generation grants it",
            case.witness
        );

        // Re-observing the same generation must change nothing
        // (CodexShim.Binding.Shim.observePublish_idempotent).
        let settled = shim.clone();
        shim.observe_publish(runnable.iter().copied(), || host_can_listen);
        assert_eq!(
            shim, settled,
            "{}: re-observing one generation must be idempotent",
            case.witness
        );
    }
}

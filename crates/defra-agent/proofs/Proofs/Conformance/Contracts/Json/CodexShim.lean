import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases
import Proofs.CodexShim.LocalInterrupt
import Proofs.CodexShim.Binding

/-!
# Codex Shim Contract JSON

Finite adapter projection vectors for the stock Codex app-server surface
exposed by the DEFRA Codex shim.
-/

namespace Conformance.Contracts

open Conformance.ContractCases

structure CodexShimProjectionCase where
  witness : String
  leanTheorems : List String
  requestState : String
  responseStatus : Option String
  localInterruptAcked : Bool
  projectedPhase : String
  terminal : Bool
  effectivelyTerminal : Bool
  interruptibleRequestState : Bool

def codexShimProjectionCaseJson (witness : CodexShimProjectionCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"request_state\":" ++ jsonString witness.requestState ++ ","
    ++ "\"response_status\":"
      ++ jsonOptionalString witness.responseStatus ++ ","
    ++ "\"local_interrupt_acked\":"
      ++ boolString witness.localInterruptAcked ++ ","
    ++ "\"projected_phase\":" ++ jsonString witness.projectedPhase ++ ","
    ++ "\"terminal\":" ++ boolString witness.terminal ++ ","
    ++ "\"effectively_terminal\":"
      ++ boolString witness.effectivelyTerminal ++ ","
    ++ "\"interruptible_request_state\":"
      ++ boolString witness.interruptibleRequestState
    ++ "}"

def codexShimProjectionCases : List CodexShimProjectionCase :=
  [ { witness := "codex_shim.projection.pending_no_response"
    , leanTheorems :=
        [ "CodexShim.project_pending_is_in_progress"
        , "CodexShim.nonterminal_without_response_projects_in_progress"
        , "CodexShim.request_transition_projection_monotonic"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "pending"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "inProgress"
    , terminal := false
    , effectivelyTerminal := false
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.claimed_no_response"
    , leanTheorems :=
        [ "CodexShim.project_claimed_is_in_progress"
        , "CodexShim.nonterminal_without_response_projects_in_progress"
        , "CodexShim.request_transition_projection_monotonic"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "claimed"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "inProgress"
    , terminal := false
    , effectivelyTerminal := false
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.processing_streaming_response"
    , leanTheorems :=
        [ "CodexShim.project_processing_is_in_progress"
        , "CodexShim.request_transition_projection_monotonic"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "processing"
    , responseStatus := some "streaming"
    , localInterruptAcked := false
    , projectedPhase := "inProgress"
    , terminal := false
    , effectivelyTerminal := false
    , interruptibleRequestState := true
    }
  , { witness := "codex_shim.projection.nonterminal_complete_response"
    , leanTheorems :=
        [ "CodexShim.response_complete_advances_nonterminal_to_completed"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "processing"
    , responseStatus := some "complete"
    , localInterruptAcked := false
    , projectedPhase := "completed"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := true
    }
  , { witness := "codex_shim.projection.nonterminal_error_response"
    , leanTheorems :=
        [ "CodexShim.response_error_advances_nonterminal_to_failed"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "processing"
    , responseStatus := some "error"
    , localInterruptAcked := false
    , projectedPhase := "failed"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := true
    }
  , { witness := "codex_shim.projection.completed_request"
    , leanTheorems :=
        [ "CodexShim.project_completed_is_completed"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "completed"
    , responseStatus := some "error"
    , localInterruptAcked := false
    , projectedPhase := "completed"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.failed_request"
    , leanTheorems :=
        [ "CodexShim.project_failed_is_failed"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "failed"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "failed"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.dead_request"
    , leanTheorems :=
        [ "CodexShim.project_dead_is_failed"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "dead"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "failed"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.superseded_request"
    , leanTheorems :=
        [ "CodexShim.project_superseded_is_interrupted"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "superseded"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "interrupted"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.interrupted_request"
    , leanTheorems :=
        [ "CodexShim.project_interrupted_is_interrupted"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "interrupted"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "interrupted"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.local_interrupt_preempts_core_state"
    , leanTheorems :=
        [ "CodexShim.local_interrupt_projects_interrupted"
        , "CodexShim.local_interrupt_never_projects_in_progress"
        , "CodexShim.codex_turn_terminates_precisely"
        , "CodexShim.local_interrupt_requires_interruptible"
        , "CodexShim.local_interrupt_shortcut_sound"
        ]
    , requestState := "processing"
    , responseStatus := some "streaming"
    , localInterruptAcked := true
    , projectedPhase := "interrupted"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := true
    }
  , { witness := "codex_shim.projection.local_interrupt_input_required"
    , leanTheorems :=
        [ "CodexShim.local_interrupt_projects_interrupted"
        , "CodexShim.local_interrupt_never_projects_in_progress"
        , "CodexShim.codex_turn_terminates_precisely"
        , "CodexShim.local_interrupt_requires_interruptible"
        , "CodexShim.local_interrupt_shortcut_sound"
        ]
    , requestState := "inputRequired"
    , responseStatus := none
    , localInterruptAcked := true
    , projectedPhase := "interrupted"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := true
    }
  ]

def codexShimProjectionCasesJson : String :=
  jsonArray (codexShimProjectionCases.map codexShimProjectionCaseJson)

structure CodexShimSubagentToolCase where
  witness : String
  leanTheorems : List String
  toolName : String
  projectedItemKind : String
  collabTool : Option String
  reciprocalLink : Bool
  projectionSettled : Bool

def codexShimSubagentToolCaseJson
    (witness : CodexShimSubagentToolCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"tool_name\":" ++ jsonString witness.toolName ++ ","
    ++ "\"projected_item_kind\":" ++ jsonString witness.projectedItemKind ++ ","
    ++ "\"collab_tool\":" ++ jsonOptionalString witness.collabTool ++ ","
    ++ "\"reciprocal_link\":" ++ boolString witness.reciprocalLink ++ ","
    ++ "\"projection_settled\":" ++ boolString witness.projectionSettled
    ++ "}"

def codexShimSubagentToolCases : List CodexShimSubagentToolCase :=
  [ { witness := "codex_shim.subagent_tool.spawn"
    , leanTheorems := [ "CodexShim.known_subagent_control_projects_collab" ]
    , toolName := "spawn_subagent"
    , projectedItemKind := "collabAgentToolCall"
    , collabTool := some "spawnAgent"
    , reciprocalLink := true
    , projectionSettled := false
    }
  , { witness := "codex_shim.subagent_tool.wait"
    , leanTheorems := [ "CodexShim.known_subagent_control_projects_collab" ]
    , toolName := "wait_subagent"
    , projectedItemKind := "collabAgentToolCall"
    , collabTool := some "wait"
    , reciprocalLink := true
    , projectionSettled := false
    }
  , { witness := "codex_shim.subagent_tool.steer"
    , leanTheorems := [ "CodexShim.known_subagent_control_projects_collab" ]
    , toolName := "steer_subagent"
    , projectedItemKind := "collabAgentToolCall"
    , collabTool := some "sendInput"
    , reciprocalLink := true
    , projectionSettled := false
    }
  , { witness := "codex_shim.subagent_tool.cancel"
    , leanTheorems := [ "CodexShim.known_subagent_control_projects_collab" ]
    , toolName := "cancel_subagent"
    , projectedItemKind := "collabAgentToolCall"
    , collabTool := some "closeAgent"
    , reciprocalLink := true
    , projectionSettled := false
    }
  , { witness := "codex_shim.subagent_tool.list"
    , leanTheorems := [ "CodexShim.non_control_tool_stays_mcp" ]
    , toolName := "list_subagents"
    , projectedItemKind := "mcpToolCall"
    , collabTool := none
    , reciprocalLink := false
    , projectionSettled := false
    }
  , { witness := "codex_shim.subagent_tool.read"
    , leanTheorems := [ "CodexShim.non_control_tool_stays_mcp" ]
    , toolName := "read_subagent"
    , projectedItemKind := "mcpToolCall"
    , collabTool := none
    , reciprocalLink := false
    , projectionSettled := false
    }
  , { witness := "codex_shim.subagent_tool.unresolved_open"
    , leanTheorems := [ "CodexShim.unresolved_subagent_control_defers_while_open" ]
    , toolName := "spawn_subagent"
    , projectedItemKind := "deferred"
    , collabTool := none
    , reciprocalLink := false
    , projectionSettled := false
    }
  , { witness := "codex_shim.subagent_tool.unresolved_settled"
    , leanTheorems :=
        [ "CodexShim.settled_unresolved_subagent_control_stays_visible" ]
    , toolName := "spawn_subagent"
    , projectedItemKind := "mcpToolCall"
    , collabTool := none
    , reciprocalLink := false
    , projectionSettled := true
    }
  ]

def codexShimSubagentToolCasesJson : String :=
  jsonArray (codexShimSubagentToolCases.map codexShimSubagentToolCaseJson)

structure CodexShimSubagentStatusCase where
  witness : String
  leanTheorems : List String
  requestState : String
  projectedAgentStatus : String
  terminal : Bool

def codexShimSubagentStatusCaseJson
    (witness : CodexShimSubagentStatusCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"request_state\":" ++ jsonString witness.requestState ++ ","
    ++ "\"projected_agent_status\":"
      ++ jsonString witness.projectedAgentStatus ++ ","
    ++ "\"terminal\":" ++ boolString witness.terminal
    ++ "}"

def codexShimSubagentStatusCases : List CodexShimSubagentStatusCase :=
  [ { witness := "codex_shim.subagent_status.pending"
    , leanTheorems := [ "CodexShim.subagent_status_terminal_precisely" ]
    , requestState := "pending", projectedAgentStatus := "pendingInit", terminal := false }
  , { witness := "codex_shim.subagent_status.claimed"
    , leanTheorems := [ "CodexShim.subagent_status_terminal_precisely" ]
    , requestState := "claimed", projectedAgentStatus := "running", terminal := false }
  , { witness := "codex_shim.subagent_status.processing"
    , leanTheorems := [ "CodexShim.subagent_status_terminal_precisely" ]
    , requestState := "processing", projectedAgentStatus := "running", terminal := false }
  , { witness := "codex_shim.subagent_status.input_required"
    , leanTheorems := [ "CodexShim.subagent_status_terminal_precisely" ]
    , requestState := "inputRequired", projectedAgentStatus := "running", terminal := false }
  , { witness := "codex_shim.subagent_status.completed"
    , leanTheorems := [ "CodexShim.subagent_status_terminal_precisely" ]
    , requestState := "completed", projectedAgentStatus := "completed", terminal := true }
  , { witness := "codex_shim.subagent_status.failed"
    , leanTheorems := [ "CodexShim.subagent_status_terminal_precisely" ]
    , requestState := "failed", projectedAgentStatus := "errored", terminal := true }
  , { witness := "codex_shim.subagent_status.dead"
    , leanTheorems := [ "CodexShim.subagent_status_terminal_precisely" ]
    , requestState := "dead", projectedAgentStatus := "errored", terminal := true }
  , { witness := "codex_shim.subagent_status.superseded"
    , leanTheorems := [ "CodexShim.subagent_status_terminal_precisely" ]
    , requestState := "superseded", projectedAgentStatus := "interrupted", terminal := true }
  , { witness := "codex_shim.subagent_status.interrupted"
    , leanTheorems := [ "CodexShim.subagent_status_terminal_precisely" ]
    , requestState := "interrupted", projectedAgentStatus := "interrupted", terminal := true }
  ]

def codexShimSubagentStatusCasesJson : String :=
  jsonArray (codexShimSubagentStatusCases.map codexShimSubagentStatusCaseJson)

structure CodexShimSubagentVisibilityCase where
  witness : String
  leanTheorems : List String
  authorized : Bool
  loaded : Bool
  projectionMode : String

def codexShimSubagentVisibilityCaseJson
    (witness : CodexShimSubagentVisibilityCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"authorized\":" ++ boolString witness.authorized ++ ","
    ++ "\"loaded\":" ++ boolString witness.loaded ++ ","
    ++ "\"projection_mode\":" ++ jsonString witness.projectionMode
    ++ "}"

def codexShimSubagentVisibilityCases : List CodexShimSubagentVisibilityCase :=
  [ { witness := "codex_shim.subagent_visibility.unauthorized_unloaded"
    , leanTheorems := [ "CodexShim.unauthorized_child_thread_is_hidden" ]
    , authorized := false
    , loaded := false
    , projectionMode := "hidden"
    }
  , { witness := "codex_shim.subagent_visibility.unauthorized_loaded"
    , leanTheorems := [ "CodexShim.unauthorized_child_thread_is_hidden" ]
    , authorized := false
    , loaded := true
    , projectionMode := "hidden"
    }
  , { witness := "codex_shim.subagent_visibility.authorized_unloaded"
    , leanTheorems := [ "CodexShim.authorized_unloaded_child_is_snapshot" ]
    , authorized := true
    , loaded := false
    , projectionMode := "snapshot"
    }
  , { witness := "codex_shim.subagent_visibility.authorized_loaded"
    , leanTheorems := [ "CodexShim.authorized_loaded_child_is_live" ]
    , authorized := true
    , loaded := true
    , projectionMode := "live"
    }
  ]

def codexShimSubagentVisibilityCasesJson : String :=
  jsonArray
    (codexShimSubagentVisibilityCases.map codexShimSubagentVisibilityCaseJson)

structure CodexShimSubagentMetadataCase where
  witness : String
  leanTheorems : List String
  runtimeModel : Option String
  runtimeReasoningEffort : Option String
  projectedModel : Option String
  projectedReasoningEffort : Option String

def codexShimSubagentMetadataCaseJson
    (witness : CodexShimSubagentMetadataCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"runtime_model\":" ++ jsonOptionalString witness.runtimeModel ++ ","
    ++ "\"runtime_reasoning_effort\":"
      ++ jsonOptionalString witness.runtimeReasoningEffort ++ ","
    ++ "\"projected_model\":"
      ++ jsonOptionalString witness.projectedModel ++ ","
    ++ "\"projected_reasoning_effort\":"
      ++ jsonOptionalString witness.projectedReasoningEffort
    ++ "}"

def codexShimSubagentMetadataCases : List CodexShimSubagentMetadataCase :=
  [ { witness := "codex_shim.subagent_metadata.runtime_model"
    , leanTheorems := [ "CodexShim.collab_model_is_runtime_model" ]
    , runtimeModel := some "child-model"
    , runtimeReasoningEffort := none
    , projectedModel := some "child-model"
    , projectedReasoningEffort := none
    }
  , { witness := "codex_shim.subagent_metadata.absent_values"
    , leanTheorems :=
        [ "CodexShim.collab_model_is_runtime_model"
        , "CodexShim.absent_runtime_reasoning_effort_stays_absent"
        ]
    , runtimeModel := none
    , runtimeReasoningEffort := none
    , projectedModel := none
    , projectedReasoningEffort := none
    }
  ]

def codexShimSubagentMetadataCasesJson : String :=
  jsonArray
    (codexShimSubagentMetadataCases.map codexShimSubagentMetadataCaseJson)

structure CodexShimSubagentListingCase where
  witness : String
  leanTheorems : List String
  sourceKind : String
  authorized : Bool
  listed : Bool

def codexShimSubagentListingCaseJson
    (witness : CodexShimSubagentListingCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"source_kind\":" ++ jsonString witness.sourceKind ++ ","
    ++ "\"authorized\":" ++ boolString witness.authorized ++ ","
    ++ "\"listed\":" ++ boolString witness.listed
    ++ "}"

def codexShimSubagentListingCases : List CodexShimSubagentListingCase :=
  [ { witness := "codex_shim.subagent_listing.generic"
    , leanTheorems :=
        [ "CodexShim.authorized_generic_subagent_filter_lists_child" ]
    , sourceKind := "subAgent"
    , authorized := true
    , listed := true
    }
  , { witness := "codex_shim.subagent_listing.thread_spawn"
    , leanTheorems :=
        [ "CodexShim.authorized_thread_spawn_filter_lists_child" ]
    , sourceKind := "subAgentThreadSpawn"
    , authorized := true
    , listed := true
    }
  , { witness := "codex_shim.subagent_listing.cli"
    , leanTheorems := [ "CodexShim.cli_filter_does_not_list_spawned_child" ]
    , sourceKind := "cli"
    , authorized := true
    , listed := false
    }
  , { witness := "codex_shim.subagent_listing.review"
    , leanTheorems := [ "CodexShim.review_filter_does_not_list_spawned_child" ]
    , sourceKind := "subAgentReview"
    , authorized := true
    , listed := false
    }
  , { witness := "codex_shim.subagent_listing.unauthorized"
    , leanTheorems := [ "CodexShim.unauthorized_child_never_listed" ]
    , sourceKind := "subAgentThreadSpawn"
    , authorized := false
    , listed := false
    }
  ]

def codexShimSubagentListingCasesJson : String :=
  jsonArray
    (codexShimSubagentListingCases.map codexShimSubagentListingCaseJson)

structure CodexShimSubagentThreadShapeCase where
  witness : String
  leanTheorems : List String
  parentThreadId : String
  nativeSourceParent : Option String
  legacyTopLevelParent : Option String
  replayStages : List String

def codexShimSubagentThreadShapeCaseJson
    (witness : CodexShimSubagentThreadShapeCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"parent_thread_id\":" ++ jsonString witness.parentThreadId ++ ","
    ++ "\"native_source_parent\":"
      ++ jsonOptionalString witness.nativeSourceParent ++ ","
    ++ "\"legacy_top_level_parent\":"
      ++ jsonOptionalString witness.legacyTopLevelParent ++ ","
    ++ "\"replay_stages\":" ++ jsonStringArray witness.replayStages
    ++ "}"

def codexShimSubagentThreadShapeCases : List CodexShimSubagentThreadShapeCase :=
  [ { witness := "codex_shim.subagent_thread.native_shape"
    , leanTheorems :=
        [ "CodexShim.subagent_parent_uses_native_source"
        , "CodexShim.subagent_parent_omits_legacy_top_level"
        , "CodexShim.completed_compaction_replay_matches_runtime_order"
        ]
    , parentThreadId := "parent-thread"
    , nativeSourceParent := some "parent-thread"
    , legacyTopLevelParent := none
    , replayStages := ["user", "compaction", "modelItems"]
    }
  ]

def codexShimSubagentThreadShapeCasesJson : String :=
  jsonArray
    (codexShimSubagentThreadShapeCases.map codexShimSubagentThreadShapeCaseJson)

structure CodexShimContextUsageCase where
  witness : String
  leanTheorems : List String
  cumulativeInput : Nat
  cumulativeOutput : Nat
  latestPrompt : Nat
  latestCompletion : Nat
  modelWindow : Nat
  totalTokens : Nat
  currentContextTokens : Nat
  remainingTokens : Nat

def codexShimContextUsageCaseJson
    (witness : CodexShimContextUsageCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"cumulative_input\":" ++ toString witness.cumulativeInput ++ ","
    ++ "\"cumulative_output\":" ++ toString witness.cumulativeOutput ++ ","
    ++ "\"latest_prompt\":" ++ toString witness.latestPrompt ++ ","
    ++ "\"latest_completion\":" ++ toString witness.latestCompletion ++ ","
    ++ "\"model_window\":" ++ toString witness.modelWindow ++ ","
    ++ "\"total_tokens\":" ++ toString witness.totalTokens ++ ","
    ++ "\"current_context_tokens\":"
      ++ toString witness.currentContextTokens ++ ","
    ++ "\"remaining_tokens\":" ++ toString witness.remainingTokens
    ++ "}"

def codexShimContextUsageCases : List CodexShimContextUsageCase :=
  [ { witness := "codex_shim.context.latest_call_not_cumulative"
    , leanTheorems :=
        [ "CodexShim.current_context_uses_latest_call"
        , "CodexShim.context_remaining_le_window"
        ]
    , cumulativeInput := 850
    , cumulativeOutput := 150
    , latestPrompt := 300
    , latestCompletion := 20
    , modelWindow := 1000
    , totalTokens := 1000
    , currentContextTokens := 320
    , remainingTokens := 680
    }
  , { witness := "codex_shim.context.over_window_saturates_remaining"
    , leanTheorems :=
        [ "CodexShim.current_context_uses_latest_call"
        , "CodexShim.context_remaining_le_window"
        , "CodexShim.context_remaining_saturates_at_zero"
        ]
    , cumulativeInput := 1400
    , cumulativeOutput := 200
    , latestPrompt := 1100
    , latestCompletion := 50
    , modelWindow := 1000
    , totalTokens := 1600
    , currentContextTokens := 1150
    , remainingTokens := 0
    }
  ]

def codexShimContextUsageCasesJson : String :=
  jsonArray (codexShimContextUsageCases.map codexShimContextUsageCaseJson)

structure CodexShimCompactionProjectionCase where
  witness : String
  leanTheorems : List String
  previousCallState : Option String
  callState : String
  projectedEvents : List String
  claimsCompacted : Bool

def codexShimCompactionProjectionCaseJson
    (witness : CodexShimCompactionProjectionCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"previous_call_state\":"
      ++ jsonOptionalString witness.previousCallState ++ ","
    ++ "\"call_state\":" ++ jsonString witness.callState ++ ","
    ++ "\"projected_events\":" ++ jsonStringArray witness.projectedEvents ++ ","
    ++ "\"claims_compacted\":" ++ boolString witness.claimsCompacted
    ++ "}"

def codexShimCompactionProjectionCases : List CodexShimCompactionProjectionCase :=
  [ { witness := "codex_shim.compaction.queued_first_observation"
    , leanTheorems := [ "CodexShim.queued_compaction_projects_started" ]
    , previousCallState := none
    , callState := "queued"
    , projectedEvents := ["started"]
    , claimsCompacted := false
    }
  , { witness := "codex_shim.compaction.running_first_observation"
    , leanTheorems := [ "CodexShim.running_compaction_projects_started" ]
    , previousCallState := none
    , callState := "running"
    , projectedEvents := ["started"]
    , claimsCompacted := false
    }
  , { witness := "codex_shim.compaction.completed_first_observation"
    , leanTheorems :=
        [ "CodexShim.completed_first_observation_projects_lifecycle_pair" ]
    , previousCallState := none
    , callState := "completed"
    , projectedEvents := ["started", "completed"]
    , claimsCompacted := true
    }
  , { witness := "codex_shim.compaction.running_to_completed"
    , leanTheorems := [ "CodexShim.running_to_completed_projects_completion" ]
    , previousCallState := some "running"
    , callState := "completed"
    , projectedEvents := ["completed"]
    , claimsCompacted := true
    }
  , { witness := "codex_shim.compaction.failed_first_observation"
    , leanTheorems := [ "CodexShim.failed_compaction_never_claims_completed" ]
    , previousCallState := none
    , callState := "failed"
    , projectedEvents := []
    , claimsCompacted := false
    }
  , { witness := "codex_shim.compaction.cancelled_first_observation"
    , leanTheorems := [ "CodexShim.cancelled_compaction_never_claims_completed" ]
    , previousCallState := none
    , callState := "cancelled"
    , projectedEvents := []
    , claimsCompacted := false
    }
  , { witness := "codex_shim.compaction.running_to_failed"
    , leanTheorems := [ "CodexShim.running_to_failed_never_claims_completed" ]
    , previousCallState := some "running"
    , callState := "failed"
    , projectedEvents := []
    , claimsCompacted := false
    }
  , { witness := "codex_shim.compaction.running_to_cancelled"
    , leanTheorems := [ "CodexShim.running_to_cancelled_never_claims_completed" ]
    , previousCallState := some "running"
    , callState := "cancelled"
    , projectedEvents := []
    , claimsCompacted := false
    }
  ]

def codexShimCompactionProjectionCasesJson : String :=
  jsonArray
    (codexShimCompactionProjectionCases.map codexShimCompactionProjectionCaseJson)

structure CodexShimTurnLifecycleCase where
  witness : String
  leanTheorems : List String
  action : String
  prePhase : String
  postPhase : String
  preLexOrd : Nat
  postLexOrd : Nat
  monotonic : Bool

def codexShimTurnLifecycleCaseJson
    (witness : CodexShimTurnLifecycleCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"pre_phase\":" ++ jsonString witness.prePhase ++ ","
    ++ "\"post_phase\":" ++ jsonString witness.postPhase ++ ","
    ++ "\"pre_lex_ord\":" ++ toString witness.preLexOrd ++ ","
    ++ "\"post_lex_ord\":" ++ toString witness.postLexOrd ++ ","
    ++ "\"monotonic\":" ++ boolString witness.monotonic
    ++ "}"

def codexShimTurnLifecycleCases : List CodexShimTurnLifecycleCase :=
  [ { witness := "codex_shim.turn_lifecycle.start"
    , leanTheorems := [ "CodexShim.turn_lifecycle_never_regresses" ]
    , action := "start"
    , prePhase := "notStarted"
    , postPhase := "inProgress"
    , preLexOrd := 0
    , postLexOrd := 1
    , monotonic := true
    }
  , { witness := "codex_shim.turn_lifecycle.complete"
    , leanTheorems := [ "CodexShim.turn_lifecycle_never_regresses" ]
    , action := "complete"
    , prePhase := "inProgress"
    , postPhase := "completed"
    , preLexOrd := 1
    , postLexOrd := 2
    , monotonic := true
    }
  , { witness := "codex_shim.turn_lifecycle.fail"
    , leanTheorems := [ "CodexShim.turn_lifecycle_never_regresses" ]
    , action := "fail"
    , prePhase := "inProgress"
    , postPhase := "failed"
    , preLexOrd := 1
    , postLexOrd := 2
    , monotonic := true
    }
  , { witness := "codex_shim.turn_lifecycle.interrupt"
    , leanTheorems :=
        [ "CodexShim.turn_lifecycle_never_regresses"
        , "CodexShim.interrupt_from_in_progress_is_terminal"
        , "CodexShim.interrupt_step_is_terminal"
        ]
    , action := "interrupt"
    , prePhase := "inProgress"
    , postPhase := "interrupted"
    , preLexOrd := 1
    , postLexOrd := 2
    , monotonic := true
    }
  ]

def codexShimTurnLifecycleCasesJson : String :=
  jsonArray (codexShimTurnLifecycleCases.map codexShimTurnLifecycleCaseJson)

/-- Binding vectors for the runnable-gated Codex shim (defra-agent#699).

`preState`/`postState` are the shim's binding before and after it observes one
published generation. `boundBehaviorRunnable` says whether the generation carries
the shim's bound behavior as runnable. `requiresRestart` is pinned `false`
everywhere on purpose: convergence is a consequence of `publish`, and no vector
may be satisfied by restarting the process. -/
structure CodexShimBindingCase where
  witness : String
  leanTheorems : List String
  preState : String
  unboundReason : Option String
  boundBehaviorRunnable : Bool
  hostCanListen : Bool
  postState : String
  postUnboundReason : Option String
  requiresRestart : Bool

def codexShimBindingCaseJson (witness : CodexShimBindingCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"pre_state\":" ++ jsonString witness.preState ++ ","
    ++ "\"unbound_reason\":" ++ jsonOptionalString witness.unboundReason ++ ","
    ++ "\"bound_behavior_runnable\":"
      ++ boolString witness.boundBehaviorRunnable ++ ","
    ++ "\"host_can_listen\":" ++ boolString witness.hostCanListen ++ ","
    ++ "\"post_state\":" ++ jsonString witness.postState ++ ","
    ++ "\"post_unbound_reason\":"
      ++ jsonOptionalString witness.postUnboundReason ++ ","
    ++ "\"requires_restart\":" ++ boolString witness.requiresRestart
    ++ "}"

def codexShimBindingCases : List CodexShimBindingCase :=
  [ -- defra-agent#699 itself: boot on an empty store, behavior applied later.
    { witness := "codex_shim.binding.dependency_supplied_binds_without_restart"
    , leanTheorems :=
        [ "CodexShim.Binding.Shim.converges_when_dependency_published"
        , "CodexShim.Binding.Shim.observePublish_coherent"
        ]
    , preState := "unbound"
    , unboundReason := some "dependencyMissing"
    , boundBehaviorRunnable := true
    , hostCanListen := true
    , postState := "bound"
    , postUnboundReason := none
    , requiresRestart := false
    }
    -- The behavior still is not runnable: stay unbound, keep waiting.
  , { witness := "codex_shim.binding.dependency_still_missing_stays_unbound"
    , leanTheorems :=
        [ "CodexShim.Binding.Shim.never_binds_unrunnable"
        ]
    , preState := "unbound"
    , unboundReason := some "dependencyMissing"
    , boundBehaviorRunnable := false
    , hostCanListen := true
    , postState := "unbound"
    , postUnboundReason := some "dependencyMissing"
    , requiresRestart := false
    }
    -- The dependency arrived, but the port is gone. That is not a document, so
    -- it degrades to the non-converging class rather than spinning.
  , { witness := "codex_shim.binding.listen_failure_degrades_to_host_resource"
    , leanTheorems :=
        [ "CodexShim.Binding.Shim.listen_failure_degrades_to_host_resource"
        , "CodexShim.Binding.Shim.host_resource_is_fixpoint"
        ]
    , preState := "unbound"
    , unboundReason := some "dependencyMissing"
    , boundBehaviorRunnable := true
    , hostCanListen := false
    , postState := "unbound"
    , postUnboundReason := some "hostResource"
    , requiresRestart := false
    }
    -- A taken port is not a document. No generation resurrects it, so the
    -- runtime must not spin retrying it.
  , { witness := "codex_shim.binding.host_resource_is_nonconverging_fixpoint"
    , leanTheorems :=
        [ "CodexShim.Binding.Shim.host_resource_is_fixpoint"
        ]
    , preState := "unbound"
    , unboundReason := some "hostResource"
    , boundBehaviorRunnable := true
    , hostCanListen := true
    , postState := "unbound"
    , postUnboundReason := some "hostResource"
    , requiresRestart := false
    }
    -- A live listener is never torn down by a later generation.
  , { witness := "codex_shim.binding.bound_never_unbinds_on_republish"
    , leanTheorems :=
        [ "CodexShim.Binding.Shim.bound_never_unbinds"
        , "CodexShim.Binding.Shim.bound_is_absorbing"
        , "CodexShim.Binding.Shim.observePublish_idempotent"
        ]
    , preState := "bound"
    , unboundReason := none
    , boundBehaviorRunnable := true
    , hostCanListen := true
    , postState := "bound"
    , postUnboundReason := none
    , requiresRestart := false
    }
  ]

def codexShimBindingCasesJson : String :=
  jsonArray (codexShimBindingCases.map codexShimBindingCaseJson)

end Conformance.Contracts

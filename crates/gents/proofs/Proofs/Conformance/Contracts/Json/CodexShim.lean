import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases
import Proofs.CodexShim.LocalInterrupt
import Proofs.CodexShim.Binding

namespace Conformance.Contracts

open Conformance.ContractCases

structure CodexShimProjectionCase where
  witness : String
  leanTheorems : List String
  requestState : String
  isSuperseded : Bool
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
    ++ "\"is_superseded\":" ++ boolString witness.isSuperseded ++ ","
    ++ "\"local_interrupt_acked\":"
      ++ boolString witness.localInterruptAcked ++ ","
    ++ "\"projected_phase\":" ++ jsonString witness.projectedPhase ++ ","
    ++ "\"terminal\":" ++ boolString witness.terminal ++ ","
    ++ "\"effectively_terminal\":"
      ++ boolString witness.effectivelyTerminal ++ ","
    ++ "\"interruptible_request_state\":"
      ++ boolString witness.interruptibleRequestState
    ++ "}"

def genericClientProjectionTheorems : List String :=
  [ "deriveAttempt_total"
  , "deriveAttempt_request_mapping"
  , "projectHead_requestState"
  , "lifecycle_transition_monotonic"
  , "supersession_override_monotonic"
  , "terminal_coherence"
  , "CodexShim.projectClientTurnState_terminal"
  , "CodexShim.projection_without_local_interrupt"
  , "CodexShim.codex_turn_terminates_precisely"
  ]

def localInterruptProjectionTheorems : List String :=
  [ "CodexShim.local_interrupt_projects_interrupted"
  , "CodexShim.local_interrupt_never_projects_in_progress"
  , "CodexShim.codex_turn_terminates_precisely"
  , "CodexShim.local_interrupt_requires_interruptible"
  , "CodexShim.local_interrupt_shortcut_sound"
  ]

def codexShimProjectionCase
    (witness : String)
    (requestState : RequestState)
    (localInterruptAcked : Bool)
    (isSuperseded : Bool := false) : CodexShimProjectionCase :=
  let observation : CodexShim.ProjectionObservation :=
    { requestState := requestState
    , isSuperseded := isSuperseded
    , localInterruptAcked := localInterruptAcked
    }
  let phase := CodexShim.projectObservation observation
  { witness := witness
  , leanTheorems :=
      if localInterruptAcked then
        localInterruptProjectionTheorems
      else
        genericClientProjectionTheorems
  , requestState := requestState.toDefraDB
  , isSuperseded := isSuperseded
  , localInterruptAcked := localInterruptAcked
  , projectedPhase := phase.toProtocol
  , terminal := decide (CodexShim.TurnPhase.terminal phase)
  , effectivelyTerminal := decide (CodexShim.turnEffectivelyTerminal observation)
  , interruptibleRequestState := decide (CodexShim.interruptibleRequestState requestState)
  }

def codexShimProjectionCases : List CodexShimProjectionCase :=
  [ codexShimProjectionCase "codex_shim.projection.workspace_binding_pending"
      .workspaceBindingPending false
  , codexShimProjectionCase "codex_shim.projection.pending"
      .pending false
  , codexShimProjectionCase "codex_shim.projection.claimed"
      .claimed false
  , codexShimProjectionCase "codex_shim.projection.processing"
      .processing false
  , codexShimProjectionCase "codex_shim.projection.completed_request"
      .completed false
  , codexShimProjectionCase "codex_shim.projection.failed_request"
      .failed false
  , codexShimProjectionCase "codex_shim.projection.dead_request"
      .dead false
  , codexShimProjectionCase "codex_shim.projection.superseded_request"
      .superseded false
  , codexShimProjectionCase "codex_shim.projection.supersession_override"
      .processing false true
  , codexShimProjectionCase "codex_shim.projection.interrupted_request"
      .interrupted false
  , codexShimProjectionCase "codex_shim.projection.local_interrupt_preempts_core_state"
      .processing true
  ]

def codexShimProjectionCasesJson : String :=
  jsonArray (codexShimProjectionCases.map codexShimProjectionCaseJson)

structure CodexShimReasoningProjectionCase where
  witness : String
  leanTheorems : List String
  itemOpen : Bool
  itemCompleted : Bool
  cursorPrimed : Bool
  streamedText : Option String
  liveDelta : Option String
  durableText : Option String
  terminal : Bool
  projectedEvents : List String
  projectedDelta : Option String
  completedText : Option String

def reasoningProjectionEventName : CodexShim.ReasoningProjectionEvent → String
  | .started => "started"
  | .rawTextDelta _ => "rawTextDelta"
  | .completed => "completed"

def codexShimReasoningProjectionCase
    (witness : String)
    (leanTheorems : List String)
    (observation : CodexShim.ReasoningProjectionObservation) :
    CodexShimReasoningProjectionCase :=
  { witness := witness
  , leanTheorems := leanTheorems
  , itemOpen := observation.itemOpen
  , itemCompleted := observation.itemCompleted
  , cursorPrimed := observation.cursorPrimed
  , streamedText := observation.streamedText
  , liveDelta := observation.liveDelta
  , durableText := observation.durableText
  , terminal := observation.terminal
  , projectedEvents :=
      (CodexShim.reasoningProjectionEvents observation).map
        reasoningProjectionEventName
  , projectedDelta := CodexShim.reasoningTextForObservation observation
  , completedText := CodexShim.completedReasoningText observation
  }

def codexShimReasoningProjectionCaseJson
    (witness : CodexShimReasoningProjectionCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"item_open\":" ++ boolString witness.itemOpen ++ ","
    ++ "\"item_completed\":" ++ boolString witness.itemCompleted ++ ","
    ++ "\"cursor_primed\":" ++ boolString witness.cursorPrimed ++ ","
    ++ "\"streamed_text\":" ++ jsonOptionalString witness.streamedText ++ ","
    ++ "\"live_delta\":" ++ jsonOptionalString witness.liveDelta ++ ","
    ++ "\"durable_text\":" ++ jsonOptionalString witness.durableText ++ ","
    ++ "\"terminal\":" ++ boolString witness.terminal ++ ","
    ++ "\"projected_events\":" ++ jsonStringArray witness.projectedEvents ++ ","
    ++ "\"projected_delta\":" ++ jsonOptionalString witness.projectedDelta ++ ","
    ++ "\"completed_text\":" ++ jsonOptionalString witness.completedText
    ++ "}"

def codexShimReasoningProjectionCases : List CodexShimReasoningProjectionCase :=
  [ codexShimReasoningProjectionCase
      "codex_shim.reasoning.first_live"
      [ "CodexShim.first_live_reasoning_projects_raw_lifecycle" ]
      { itemOpen := false
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := none
      , liveDelta := some "inspect"
      , durableText := none
      , terminal := false }
  , codexShimReasoningProjectionCase
      "codex_shim.reasoning.append_live"
      [ "CodexShim.appended_live_reasoning_projects_only_delta" ]
      { itemOpen := true
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := some "inspect"
      , liveDelta := some " then test"
      , durableText := none
      , terminal := false }
  , codexShimReasoningProjectionCase
      "codex_shim.reasoning.resumed_unchanged"
      [ "CodexShim.primed_resume_without_new_reasoning_replays_nothing" ]
      { itemOpen := true
      , itemCompleted := false
      , cursorPrimed := true
      , streamedText := some "already visible"
      , liveDelta := none
      , durableText := none
      , terminal := false }
  , codexShimReasoningProjectionCase
      "codex_shim.reasoning.terminal_open"
      [ "CodexShim.terminal_open_reasoning_completes_without_replay"
      , "CodexShim.terminal_completed_item_uses_durable_reasoning"
      ]
      { itemOpen := true
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := some "inspect then test"
      , liveDelta := none
      , durableText := some "inspect then test"
      , terminal := true }
  , codexShimReasoningProjectionCase
      "codex_shim.reasoning.terminal_first_observation"
      [ "CodexShim.terminal_durable_first_observation_projects_lifecycle"
      , "CodexShim.terminal_completed_item_uses_durable_reasoning"
      ]
      { itemOpen := false
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := none
      , liveDelta := none
      , durableText := some "inspect then test"
      , terminal := true }
  , codexShimReasoningProjectionCase
      "codex_shim.reasoning.absent"
      [ "CodexShim.absent_reasoning_projects_nothing" ]
      { itemOpen := false
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := none
      , liveDelta := none
      , durableText := none
      , terminal := true }
  , codexShimReasoningProjectionCase
      "codex_shim.reasoning.terminal_durable_suffix"
      [ "CodexShim.terminal_durable_suffix_projects_before_completion"
      , "CodexShim.terminal_completed_item_uses_durable_reasoning"
      ]
      { itemOpen := true
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := some "inspect then test"
      , liveDelta := some "; durable suffix"
      , durableText := some "inspect then test; durable suffix"
      , terminal := true }
  , codexShimReasoningProjectionCase
      "codex_shim.reasoning.terminal_streamed_fallback"
      [ "CodexShim.terminal_without_durable_reasoning_keeps_streamed_text" ]
      { itemOpen := true
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := some "streamed reasoning"
      , liveDelta := none
      , durableText := none
      , terminal := true }
  , codexShimReasoningProjectionCase
      "codex_shim.reasoning.reset_before_terminal"
      [ "CodexShim.reset_before_terminal_suppresses_durable_replay"
      , "CodexShim.reset_before_terminal_has_no_second_completed_text"
      ]
      { itemOpen := false
      , itemCompleted := true
      , cursorPrimed := false
      , streamedText := none
      , liveDelta := none
      , durableText := some "already completed reasoning"
      , terminal := true }
  ]

def codexShimReasoningProjectionCasesJson : String :=
  jsonArray
    (codexShimReasoningProjectionCases.map codexShimReasoningProjectionCaseJson)

def threadPresentationStatusName : CodexShim.ThreadPresentationStatus → String
  | .active => "active"
  | .idle => "idle"
  | .systemError => "systemError"

structure CodexShimThreadStatusCase where
  witness : String
  leanTheorems : List String
  requestState : Option String
  projectedStatus : String

def clientHeadProjection
    (requestState : RequestState) : ClientHeadProjection :=
  projectHead
    { request :=
        { lifecycleState := requestState
        , isSuperseded := false
        }
    }

def codexShimThreadStatusCase
    (witness : String)
    (leanTheorems : List String)
    (requestState : Option RequestState) : CodexShimThreadStatusCase :=
  let head := requestState.map clientHeadProjection
  { witness
  , leanTheorems
  , requestState := requestState.map RequestState.toDefraDB
  , projectedStatus :=
      threadPresentationStatusName
        (CodexShim.projectThreadStatus head)
  }

def codexShimThreadStatusCaseJson (witness : CodexShimThreadStatusCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"request_state\":" ++ jsonOptionalString witness.requestState ++ ","
    ++ "\"projected_status\":" ++ jsonString witness.projectedStatus
    ++ "}"

def codexShimThreadStatusCases : List CodexShimThreadStatusCase :=
  [ codexShimThreadStatusCase "codex_shim.thread_status.workspace_binding_pending" []
      (some .workspaceBindingPending)
  , codexShimThreadStatusCase "codex_shim.thread_status.pending" [] (some .pending)
  , codexShimThreadStatusCase "codex_shim.thread_status.claimed" [] (some .claimed)
  , codexShimThreadStatusCase
      "codex_shim.thread_status.processing"
      ["CodexShim.active_request_projects_active_thread"]
      (some .processing)
  , codexShimThreadStatusCase
      "codex_shim.thread_status.completed"
      ["CodexShim.completed_request_projects_idle_thread"]
      (some .completed)
  , codexShimThreadStatusCase
      "codex_shim.thread_status.failed"
      ["CodexShim.failed_request_projects_system_error_thread"]
      (some .failed)
  , codexShimThreadStatusCase "codex_shim.thread_status.dead" [] (some .dead)
  , codexShimThreadStatusCase "codex_shim.thread_status.superseded" []
      (some .superseded)
  , codexShimThreadStatusCase "codex_shim.thread_status.interrupted" []
      (some .interrupted)
  , codexShimThreadStatusCase
      "codex_shim.thread_status.quiescent"
      ["CodexShim.missing_request_observation_is_quiescent"]
      none
  ]

def codexShimThreadStatusCasesJson : String :=
  jsonArray (codexShimThreadStatusCases.map codexShimThreadStatusCaseJson)

structure CodexShimBehaviorSelectionCase where
  witness : String
  leanTheorems : List String
  rootBehaviorId : String
  threadBehaviorId : Option String
  projectedBehaviorId : String
  selectedOwner : String
  actualOwner : String
  actualBehavior : String
  resolvedModel : Option String
  projectedModel : Option String

def codexShimBehaviorSelectionCase
    (witness : String) (leanTheorems : List String)
    (rootBehaviorId : String) (threadBehaviorId : Option String)
    (selectedOwner actualOwner actualBehavior : String)
    (resolvedModel : Option String) : CodexShimBehaviorSelectionCase :=
  let projectedBehaviorId := CodexShim.projectionBehaviorId rootBehaviorId threadBehaviorId
  { witness, leanTheorems, rootBehaviorId, threadBehaviorId, projectedBehaviorId
  , selectedOwner, actualOwner, actualBehavior, resolvedModel
  , projectedModel := CodexShim.projectedThreadModel selectedOwner projectedBehaviorId
      actualOwner actualBehavior resolvedModel
  }

def codexShimBehaviorSelectionCaseJson
    (witness : CodexShimBehaviorSelectionCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"root_behavior_id\":" ++ jsonString witness.rootBehaviorId ++ ","
    ++ "\"thread_behavior_id\":" ++ jsonOptionalString witness.threadBehaviorId ++ ","
    ++ "\"projected_behavior_id\":" ++ jsonString witness.projectedBehaviorId ++ ","
    ++ "\"selected_owner\":" ++ jsonString witness.selectedOwner ++ ","
    ++ "\"actual_owner\":" ++ jsonString witness.actualOwner ++ ","
    ++ "\"actual_behavior\":" ++ jsonString witness.actualBehavior ++ ","
    ++ "\"resolved_model\":" ++ jsonOptionalString witness.resolvedModel ++ ","
    ++ "\"projected_model\":" ++ jsonOptionalString witness.projectedModel
    ++ "}"

def codexShimBehaviorSelectionCases : List CodexShimBehaviorSelectionCase :=
  [ codexShimBehaviorSelectionCase "codex_shim.behavior.child"
      ["CodexShim.exact_binding_supplies_model"]
      "root" (some "child") "child-owner" "child-owner" "child" (some "child-model")
  , codexShimBehaviorSelectionCase "codex_shim.behavior.root"
      ["CodexShim.absent_child_behavior_keeps_root_response_metadata"]
      "root" none "root-owner" "root-owner" "root" (some "root-model")
  , codexShimBehaviorSelectionCase "codex_shim.behavior.unavailable"
      ["CodexShim.unavailable_binding_stays_unavailable"]
      "root" (some "child") "child-owner" "child-owner" "child" none
  , codexShimBehaviorSelectionCase "codex_shim.behavior.foreign_owner"
      ["CodexShim.foreign_owner_cannot_supply_model"]
      "root" (some "child") "child-owner" "root-owner" "child" (some "wrong-model")
  , codexShimBehaviorSelectionCase "codex_shim.behavior.foreign_behavior"
      ["CodexShim.foreign_behavior_cannot_supply_model"]
      "root" (some "child") "child-owner" "child-owner" "root" (some "wrong-model")
  ]

def codexShimBehaviorSelectionCasesJson : String :=
  jsonArray
    (codexShimBehaviorSelectionCases.map codexShimBehaviorSelectionCaseJson)

structure CodexShimToolMetadataCase where
  witness : String
  leanTheorems : List String
  fallbackServer : String
  selectedServer : Option String
  fallbackTool : String
  selectedTool : Option String
  denialReason : Option String
  cancelCause : Option String
  failureClass : Option String
  resultFallback : Option String
  latencyMs : Option Nat
  startedAtMs : Option Nat
  completedAtMs : Option Nat
  persistedEventAtMs : Option Nat
  observedAtMs : Nat
  projectedServer : String
  projectedTool : String
  projectedFailure : Option String
  projectedDurationMs : Option Nat
  projectedEventAtMs : Nat

def codexShimToolMetadataCase
    (witness : String)
    (leanTheorems : List String)
    (fallbackServer : String := "gents")
    (selectedServer : Option String := none)
    (fallbackTool : String := "tool")
    (selectedTool : Option String := none)
    (denialReason : Option String := none)
    (cancelCause : Option String := none)
    (failureClass : Option String := none)
    (resultFallback : Option String := none)
    (latencyMs : Option Nat := none)
    (startedAtMs : Option Nat := none)
    (completedAtMs : Option Nat := none)
    (persistedEventAtMs : Option Nat := none)
    (observedAtMs : Nat := 200) : CodexShimToolMetadataCase :=
  { witness
  , leanTheorems
  , fallbackServer
  , selectedServer
  , fallbackTool
  , selectedTool
  , denialReason
  , cancelCause
  , failureClass
  , resultFallback
  , latencyMs
  , startedAtMs
  , completedAtMs
  , persistedEventAtMs
  , observedAtMs
  , projectedServer := CodexShim.projectedToolIdentity fallbackServer selectedServer
  , projectedTool := CodexShim.projectedToolIdentity fallbackTool selectedTool
  , projectedFailure :=
      CodexShim.projectedToolFailure denialReason cancelCause failureClass resultFallback
  , projectedDurationMs :=
      CodexShim.projectedDurationMs latencyMs startedAtMs completedAtMs
  , projectedEventAtMs :=
      CodexShim.projectedEventTimestampMs persistedEventAtMs observedAtMs
  }

def codexShimToolMetadataCaseJson (witness : CodexShimToolMetadataCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"fallback_server\":" ++ jsonString witness.fallbackServer ++ ","
    ++ "\"selected_server\":" ++ jsonOptionalString witness.selectedServer ++ ","
    ++ "\"fallback_tool\":" ++ jsonString witness.fallbackTool ++ ","
    ++ "\"selected_tool\":" ++ jsonOptionalString witness.selectedTool ++ ","
    ++ "\"denial_reason\":" ++ jsonOptionalString witness.denialReason ++ ","
    ++ "\"cancel_cause\":" ++ jsonOptionalString witness.cancelCause ++ ","
    ++ "\"failure_class\":" ++ jsonOptionalString witness.failureClass ++ ","
    ++ "\"result_fallback\":" ++ jsonOptionalString witness.resultFallback ++ ","
    ++ "\"latency_ms\":" ++ jsonOptionalNat witness.latencyMs ++ ","
    ++ "\"started_at_ms\":" ++ jsonOptionalNat witness.startedAtMs ++ ","
    ++ "\"completed_at_ms\":" ++ jsonOptionalNat witness.completedAtMs ++ ","
    ++ "\"persisted_event_at_ms\":" ++ jsonOptionalNat witness.persistedEventAtMs ++ ","
    ++ "\"observed_at_ms\":" ++ toString witness.observedAtMs ++ ","
    ++ "\"projected_server\":" ++ jsonString witness.projectedServer ++ ","
    ++ "\"projected_tool\":" ++ jsonString witness.projectedTool ++ ","
    ++ "\"projected_failure\":" ++ jsonOptionalString witness.projectedFailure ++ ","
    ++ "\"projected_duration_ms\":" ++ jsonOptionalNat witness.projectedDurationMs ++ ","
    ++ "\"projected_event_at_ms\":" ++ toString witness.projectedEventAtMs
    ++ "}"

def codexShimToolMetadataCases : List CodexShimToolMetadataCase :=
  [ codexShimToolMetadataCase
      "codex_shim.tool_metadata.selected_identity"
      ["CodexShim.selected_tool_identity_overrides_model_facing_name"]
      (selectedServer := some "service-a") (fallbackTool := "alias")
      (selectedTool := some "native-name")
  , codexShimToolMetadataCase
      "codex_shim.tool_metadata.fallback_identity"
      ["CodexShim.absent_selected_tool_identity_keeps_fallback"]
      (fallbackTool := "alias")
  , codexShimToolMetadataCase
      "codex_shim.tool_metadata.denial"
      ["CodexShim.denial_diagnostic_has_priority"]
      (denialReason := some "policy denied") (cancelCause := some "interrupted")
      (failureClass := some "policyDenied") (resultFallback := some "generic result")
  , codexShimToolMetadataCase
      "codex_shim.tool_metadata.cancel"
      ["CodexShim.cancellation_diagnostic_precedes_failure_class"]
      (cancelCause := some "deadline") (failureClass := some "timedOut")
  , codexShimToolMetadataCase
      "codex_shim.tool_metadata.result_diagnostic"
      ["CodexShim.result_diagnostic_precedes_failure_class_fallback"]
      (failureClass := some "argumentInvalid") (resultFallback := some "generic result")
  , codexShimToolMetadataCase
      "codex_shim.tool_metadata.failure_class_fallback"
      ["CodexShim.failure_class_fills_absent_result_diagnostic"]
      (failureClass := some "argumentInvalid")
  , codexShimToolMetadataCase
      "codex_shim.tool_metadata.persisted_latency"
      ["CodexShim.persisted_latency_precedes_timestamp_duration"]
      (latencyMs := some 7) (startedAtMs := some 100) (completedAtMs := some 125)
  , codexShimToolMetadataCase
      "codex_shim.tool_metadata.timestamp_duration"
      ["CodexShim.timestamp_duration_fills_absent_latency"]
      (startedAtMs := some 100) (completedAtMs := some 125)
  , codexShimToolMetadataCase
      "codex_shim.tool_metadata.absent_duration"
      ["CodexShim.incomplete_timestamps_do_not_invent_duration"]
      (startedAtMs := some 100)
  , codexShimToolMetadataCase
      "codex_shim.tool_metadata.persisted_event_time"
      ["CodexShim.persisted_event_timestamp_precedes_observation"]
      (persistedEventAtMs := some 100)
  , codexShimToolMetadataCase
      "codex_shim.tool_metadata.observed_event_time"
      ["CodexShim.absent_event_timestamp_uses_observation"]
  ]

def codexShimToolMetadataCasesJson : String :=
  jsonArray (codexShimToolMetadataCases.map codexShimToolMetadataCaseJson)

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
  [
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

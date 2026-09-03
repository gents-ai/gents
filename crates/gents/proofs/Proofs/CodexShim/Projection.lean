import Proofs.Client
import Proofs.CodexShim.TurnLifecycle
import Proofs.InferenceCall.State

namespace CodexShim

def projectClientTurnState : ClientTurnState → TurnPhase
  | .waitingForClaim | .streaming => .inProgress
  | .completed => .completed
  | .failed => .failed
  | .superseded | .interrupted => .interrupted

theorem projectClientTurnState_terminal (state : ClientTurnState) :
    TurnPhase.terminal (projectClientTurnState state) ↔ state.isTerminal = true := by
  cases state <;>
    simp [projectClientTurnState, TurnPhase.terminal, ClientTurnState.isTerminal]

inductive SubagentControlTool where
  | spawn
  | wait
  | steer
  | cancel
  | other
  deriving DecidableEq, Repr

inductive CollabTool where
  | spawnAgent
  | wait
  | sendInput
  | closeAgent
  deriving DecidableEq, Repr

def projectSubagentControl : SubagentControlTool → Option CollabTool
  | .spawn => some .spawnAgent
  | .wait => some .wait
  | .steer => some .sendInput
  | .cancel => some .closeAgent
  | .other => none

theorem known_subagent_control_projects_collab
    {tool : SubagentControlTool}
    (h : tool = .spawn ∨ tool = .wait ∨ tool = .steer ∨ tool = .cancel) :
    ∃ collab, projectSubagentControl tool = some collab := by
  rcases h with h | h | h | h <;> subst tool
  all_goals simp [projectSubagentControl]

theorem non_control_tool_stays_mcp :
    projectSubagentControl .other = none := rfl

inductive CollabToolCallPhase where
  | inProgress
  | completed
  | failed
  deriving DecidableEq, Repr

def projectCollabToolCallPhase
    (tool : SubagentControlTool)
    (hasReciprocalLink : Bool)
    (runtimePhase : CollabToolCallPhase) : CollabToolCallPhase :=
  match runtimePhase with
  | .failed => .failed
  | phase =>
      if tool == .spawn && hasReciprocalLink then .completed else phase

theorem linked_spawn_operation_completes_while_child_runs :
    projectCollabToolCallPhase .spawn true .inProgress = .completed := rfl

theorem failed_spawn_operation_stays_failed (hasReciprocalLink : Bool) :
    projectCollabToolCallPhase .spawn hasReciprocalLink .failed = .failed := rfl

theorem non_spawn_control_retains_runtime_phase
    (tool : SubagentControlTool)
    (phase : CollabToolCallPhase)
    (h : tool ≠ .spawn) :
    projectCollabToolCallPhase tool true phase = phase := by
  cases phase <;> simp [projectCollabToolCallPhase, h]

inductive SubagentItemProjection where
  | collab (tool : CollabTool)
  | mcp
  | deferred
  deriving DecidableEq, Repr

def projectSubagentItem
    (tool : SubagentControlTool)
    (hasReciprocalLink projectionSettled linkSettleExpired : Bool) :
    SubagentItemProjection :=
  match projectSubagentControl tool with
  | none => .mcp
  | some collab =>
      if hasReciprocalLink then .collab collab
      else if projectionSettled && linkSettleExpired then .mcp
      else .deferred

theorem linked_subagent_control_projects_native
    (tool : SubagentControlTool)
    (collab : CollabTool)
    (h : projectSubagentControl tool = some collab) :
    projectSubagentItem tool true false false = .collab collab := by
  simp [projectSubagentItem, h]

theorem unresolved_subagent_control_defers_while_open
    (tool : SubagentControlTool)
    (collab : CollabTool)
    (h : projectSubagentControl tool = some collab) :
    projectSubagentItem tool false false false = .deferred := by
  simp [projectSubagentItem, h]

theorem settled_unresolved_subagent_control_defers_during_link_window
    (tool : SubagentControlTool)
    (collab : CollabTool)
    (h : projectSubagentControl tool = some collab) :
    projectSubagentItem tool false true false = .deferred := by
  simp [projectSubagentItem, h]

theorem expired_unresolved_subagent_control_stays_visible
    (tool : SubagentControlTool)
    (collab : CollabTool)
    (h : projectSubagentControl tool = some collab) :
    projectSubagentItem tool false true true = .mcp := by
  simp [projectSubagentItem, h]

inductive CollabAgentPhase where
  | pendingInit
  | running
  | completed
  | errored
  | interrupted
  deriving DecidableEq, Repr

def CollabAgentPhase.terminal : CollabAgentPhase → Prop
  | .completed | .errored | .interrupted => True
  | .pendingInit | .running => False

def projectSubagentState : ClientHeadProjection → CollabAgentPhase
  | ⟨.completed, _⟩ => .completed
  | ⟨.failed, _⟩ => .errored
  | ⟨.superseded, _⟩ | ⟨.interrupted, _⟩ => .interrupted
  | ⟨.waitingForClaim, .pending⟩ => .pendingInit
  | ⟨.waitingForClaim, _⟩ | ⟨.streaming, _⟩ => .running

theorem subagent_status_terminal_precisely
    (head : ClientHeadProjection) :
    CollabAgentPhase.terminal (projectSubagentState head) ↔
      head.isTerminal = true := by
  cases head with
  | mk turnState requestState =>
    cases turnState <;> cases requestState <;>
    simp [ projectSubagentState
         , CollabAgentPhase.terminal
         , ClientHeadProjection.isTerminal
         , ClientTurnState.isTerminal
         ]

structure CollabPresentationFingerprint where
  childPhase : CollabAgentPhase
  failureMessage : Option String
  deriving DecidableEq, Repr

def collabPresentationFingerprint
    (latestChildState : ClientHeadProjection)
    (failureMessage : Option String) : CollabPresentationFingerprint :=
  { childPhase := projectSubagentState latestChildState
  , failureMessage := failureMessage
  }

theorem collab_fingerprint_includes_latest_child_state
    (state : ClientHeadProjection)
    (failureMessage : Option String) :
    (collabPresentationFingerprint state failureMessage).childPhase =
      projectSubagentState state := rfl

inductive ChildThreadProjection where
  | hidden
  | snapshot
  | live
  deriving DecidableEq, Repr

def projectChildThread (authorized loaded : Bool) : ChildThreadProjection :=
  if !authorized then .hidden
  else if loaded then .live
  else .snapshot

theorem unauthorized_child_thread_is_hidden (loaded : Bool) :
    projectChildThread false loaded = .hidden := by
  simp [projectChildThread]

theorem authorized_unloaded_child_is_snapshot :
    projectChildThread true false = .snapshot := by
  simp [projectChildThread]

theorem authorized_loaded_child_is_live :
    projectChildThread true true = .live := by
  simp [projectChildThread]

structure SubagentThreadLink where
  childRequestId : String
  childSessionId : String
  deriving DecidableEq, Repr

def receiverThreadId (link : SubagentThreadLink) : String :=
  link.childSessionId

theorem receiver_thread_is_child_session (link : SubagentThreadLink) :
    receiverThreadId link = link.childSessionId := rfl

structure CollabPresentationMetadata where
  model : Option String
  reasoningEffort : Option String
  deriving DecidableEq, Repr

def projectCollabPresentationMetadata
    (metadata : CollabPresentationMetadata) : CollabPresentationMetadata :=
  metadata

theorem collab_model_is_runtime_model (metadata : CollabPresentationMetadata) :
    (projectCollabPresentationMetadata metadata).model = metadata.model := rfl

theorem absent_runtime_reasoning_effort_stays_absent
    (model : Option String) :
    (projectCollabPresentationMetadata
      { model := model, reasoningEffort := none }).reasoningEffort = none := rfl

inductive ThreadSourceFilter where
  | cli
  | subAgent
  | subAgentReview
  | subAgentThreadSpawn
  | other
  deriving DecidableEq, Repr

def sourceFilterMatchesSpawnedSubagent : ThreadSourceFilter → Bool
  | .subAgent | .subAgentThreadSpawn => true
  | .cli | .subAgentReview | .other => false

def spawnedSubagentListed
    (authorized : Bool)
    (filters : List ThreadSourceFilter) : Bool :=
  authorized && filters.any sourceFilterMatchesSpawnedSubagent

theorem authorized_generic_subagent_filter_lists_child :
    spawnedSubagentListed true [.subAgent] = true := rfl

theorem authorized_thread_spawn_filter_lists_child :
    spawnedSubagentListed true [.subAgentThreadSpawn] = true := rfl

theorem cli_filter_does_not_list_spawned_child :
    spawnedSubagentListed true [.cli] = false := rfl

theorem review_filter_does_not_list_spawned_child :
    spawnedSubagentListed true [.subAgentReview] = false := rfl

theorem unauthorized_child_never_listed (filters : List ThreadSourceFilter) :
    spawnedSubagentListed false filters = false := by
  simp [spawnedSubagentListed]

structure SubagentThreadParentProjection where
  nativeSourceParent : Option String
  deriving DecidableEq, Repr

def projectSubagentThreadParent (parentThreadId : String) :
    SubagentThreadParentProjection :=
  { nativeSourceParent := some parentThreadId }

theorem subagent_parent_uses_native_source (parentThreadId : String) :
    (projectSubagentThreadParent parentThreadId).nativeSourceParent =
      some parentThreadId := rfl

inductive ReasoningProjectionEvent where
  | started
  | rawTextDelta (text : String)
  | completed
  deriving DecidableEq, Repr

structure ReasoningProjectionObservation where
  itemOpen : Bool
  itemCompleted : Bool
  cursorPrimed : Bool
  streamedText : Option String
  liveDelta : Option String
  durableText : Option String
  terminal : Bool
  deriving DecidableEq, Repr

def nonemptyReasoningText : Option String → Option String
  | some text => if text.isEmpty then none else some text
  | none => none

def preferReasoningText (preferred fallback : Option String) : Option String :=
  match nonemptyReasoningText preferred with
  | some text => some text
  | none => nonemptyReasoningText fallback

def reasoningTextForObservation
    (obs : ReasoningProjectionObservation) : Option String :=
  match nonemptyReasoningText obs.liveDelta with
  | some text => some text
  | none =>
      if obs.terminal && !obs.itemOpen && !obs.itemCompleted && !obs.cursorPrimed then
        nonemptyReasoningText obs.durableText
      else
        none

def reasoningProjectionEvents
    (obs : ReasoningProjectionObservation) : List ReasoningProjectionEvent :=
  let text := reasoningTextForObservation obs
  let startEvents :=
    if !obs.itemOpen && text.isSome then [.started] else []
  let deltaEvents :=
    match text with
    | some delta => [.rawTextDelta delta]
    | none => []
  let openAfter := obs.itemOpen || text.isSome
  let completionEvents :=
    if obs.terminal && openAfter && !obs.itemCompleted then [.completed] else []
  startEvents ++ deltaEvents ++ completionEvents

def completedReasoningText
    (obs : ReasoningProjectionObservation) : Option String :=
  if obs.terminal && !obs.itemCompleted then
    preferReasoningText obs.durableText obs.streamedText
  else
    none

theorem first_live_reasoning_projects_raw_lifecycle :
    reasoningProjectionEvents
      { itemOpen := false
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := none
      , liveDelta := some "inspect"
      , durableText := none
      , terminal := false } =
      [.started, .rawTextDelta "inspect"] := rfl

theorem appended_live_reasoning_projects_only_delta :
    reasoningProjectionEvents
      { itemOpen := true
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := some "inspect"
      , liveDelta := some " then test"
      , durableText := none
      , terminal := false } =
      [.rawTextDelta " then test"] := rfl

theorem primed_resume_without_new_reasoning_replays_nothing :
    reasoningProjectionEvents
      { itemOpen := true
      , itemCompleted := false
      , cursorPrimed := true
      , streamedText := some "already visible"
      , liveDelta := none
      , durableText := none
      , terminal := false } = [] := rfl

theorem terminal_open_reasoning_completes_without_replay :
    reasoningProjectionEvents
      { itemOpen := true
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := some "inspect then test"
      , liveDelta := none
      , durableText := some "inspect then test"
      , terminal := true } = [.completed] := rfl

theorem terminal_durable_first_observation_projects_lifecycle :
    reasoningProjectionEvents
      { itemOpen := false
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := none
      , liveDelta := none
      , durableText := some "inspect then test"
      , terminal := true } =
      [.started, .rawTextDelta "inspect then test", .completed] := rfl

theorem absent_reasoning_projects_nothing :
    reasoningProjectionEvents
      { itemOpen := false
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := none
      , liveDelta := none
      , durableText := none
      , terminal := true } = [] := rfl

theorem terminal_completed_item_uses_durable_reasoning :
    completedReasoningText
      { itemOpen := true
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := some "bounded preview"
      , liveDelta := none
      , durableText := some "durable reasoning"
      , terminal := true } = some "durable reasoning" := rfl

theorem terminal_without_durable_reasoning_keeps_streamed_text :
    completedReasoningText
      { itemOpen := true
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := some "streamed reasoning"
      , liveDelta := none
      , durableText := none
      , terminal := true } = some "streamed reasoning" := rfl

theorem terminal_durable_suffix_projects_before_completion :
    reasoningProjectionEvents
      { itemOpen := true
      , itemCompleted := false
      , cursorPrimed := false
      , streamedText := some "inspect then test"
      , liveDelta := some "; durable suffix"
      , durableText := some "inspect then test; durable suffix"
      , terminal := true } =
      [.rawTextDelta "; durable suffix", .completed] := rfl

theorem reset_before_terminal_suppresses_durable_replay :
    reasoningProjectionEvents
      { itemOpen := false
      , itemCompleted := true
      , cursorPrimed := false
      , streamedText := none
      , liveDelta := none
      , durableText := some "already completed reasoning"
      , terminal := true } = [] := rfl

theorem reset_before_terminal_has_no_second_completed_text :
    completedReasoningText
      { itemOpen := false
      , itemCompleted := true
      , cursorPrimed := false
      , streamedText := none
      , liveDelta := none
      , durableText := some "already completed reasoning"
      , terminal := true } = none := rfl

theorem nonterminal_without_live_reasoning_projects_nothing
    (obs : ReasoningProjectionObservation)
    (hTerminal : obs.terminal = false)
    (hDelta : nonemptyReasoningText obs.liveDelta = none) :
    reasoningProjectionEvents obs = [] := by
  simp [reasoningProjectionEvents, reasoningTextForObservation, hTerminal, hDelta]

theorem reasoning_completion_requires_an_open_item
    (obs : ReasoningProjectionObservation)
    (hClosed : obs.itemOpen = false)
    (hNoText : reasoningTextForObservation obs = none) :
    ReasoningProjectionEvent.completed ∉ reasoningProjectionEvents obs := by
  simp [reasoningProjectionEvents, hClosed, hNoText]

inductive ThreadPresentationStatus where
  | active
  | idle
  | systemError
  deriving DecidableEq, Repr

def projectThreadStatus
    (head : Option ClientHeadProjection)
    (conversationStatus : String) : ThreadPresentationStatus :=
  match head with
  | some ⟨.waitingForClaim, _⟩ | some ⟨.streaming, _⟩ => .active
  | some ⟨.failed, _⟩ => .systemError
  | some ⟨.completed, _⟩ | some ⟨.superseded, _⟩
  | some ⟨.interrupted, _⟩ => .idle
  | none => if conversationStatus = "error" then .systemError else .idle

def projectionBehaviorId (rootBehaviorId : String)
    (threadBehaviorId : Option String) : String :=
  match nonemptyReasoningText threadBehaviorId with
  | some behaviorId => behaviorId
  | none => rootBehaviorId

def projectedThreadModel (rootModel : String)
    (projectedChildModel resolvedChildModel : Option String) : String :=
  match nonemptyReasoningText resolvedChildModel with
  | some model => model
  | none =>
      match nonemptyReasoningText projectedChildModel with
      | some model => model
      | none => rootModel

def projectedToolIdentity (fallback : String) (selected : Option String) : String :=
  match nonemptyReasoningText selected with
  | some value => value
  | none => fallback

def projectedToolFailure
    (denialReason cancelCause failureClass result : Option String) : Option String :=
  match nonemptyReasoningText denialReason with
  | some value => some value
  | none =>
      match nonemptyReasoningText cancelCause with
      | some value => some value
      | none =>
          match nonemptyReasoningText result with
          | some value => some value
          | none => nonemptyReasoningText failureClass

def projectedDurationMs
    (latencyMs startedAtMs completedAtMs : Option Nat) : Option Nat :=
  match latencyMs with
  | some latency => some latency
  | none =>
      match startedAtMs, completedAtMs with
      | some started, some completed => some (completed - started)
      | _, _ => none

def projectedEventTimestampMs (persisted : Option Nat) (observed : Nat) : Nat :=
  persisted.getD observed

theorem active_request_projects_active_thread :
    projectThreadStatus (some ⟨.waitingForClaim, .processing⟩) "completed" = .active := rfl

theorem terminal_response_projects_idle_thread_before_request_terminalizes :
    projectThreadStatus (some ⟨.completed, .processing⟩) "active" = .idle := rfl

theorem failed_request_projects_system_error_thread :
    projectThreadStatus (some ⟨.failed, .failed⟩) "active" = .systemError := rfl

theorem completed_request_projects_idle_thread :
    projectThreadStatus (some ⟨.completed, .completed⟩) "error" = .idle := rfl

theorem missing_request_error_conversation_projects_system_error :
    projectThreadStatus none "error" = .systemError := rfl

theorem missing_request_active_conversation_is_quiescent :
    projectThreadStatus none "active" = .idle := rfl

theorem child_behavior_overrides_root_for_response_metadata :
    projectionBehaviorId "root" (some "child") = "child" := rfl

theorem absent_child_behavior_keeps_root_response_metadata :
    projectionBehaviorId "root" none = "root" := rfl

theorem resolved_child_model_has_priority :
    projectedThreadModel "root-model" (some "projected-child")
      (some "resolved-child") = "resolved-child" := rfl

theorem projected_child_model_fills_unavailable_behavior :
    projectedThreadModel "root-model" (some "projected-child") none =
      "projected-child" := rfl

theorem unavailable_child_model_falls_back_to_root :
    projectedThreadModel "root-model" none none = "root-model" := rfl

theorem selected_tool_identity_overrides_model_facing_name :
    projectedToolIdentity "gents" (some "service-a") = "service-a" := rfl

theorem absent_selected_tool_identity_keeps_fallback :
    projectedToolIdentity "gents" none = "gents" := rfl

theorem denial_diagnostic_has_priority :
    projectedToolFailure
      (some "policy denied") (some "interrupted") (some "policyDenied")
      (some "generic result") = some "policy denied" := rfl

theorem cancellation_diagnostic_precedes_failure_class :
    projectedToolFailure none (some "deadline") (some "timedOut") none =
      some "deadline" := rfl

theorem result_diagnostic_precedes_failure_class_fallback :
    projectedToolFailure none none (some "argumentInvalid") (some "generic result") =
      some "generic result" := rfl

theorem failure_class_fills_absent_result_diagnostic :
    projectedToolFailure none none (some "argumentInvalid") none =
      some "argumentInvalid" := rfl

theorem persisted_latency_precedes_timestamp_duration :
    projectedDurationMs (some 7) (some 100) (some 125) = some 7 := rfl

theorem timestamp_duration_fills_absent_latency :
    projectedDurationMs none (some 100) (some 125) = some 25 := rfl

theorem incomplete_timestamps_do_not_invent_duration :
    projectedDurationMs none (some 100) none = none := rfl

theorem persisted_event_timestamp_precedes_observation :
    projectedEventTimestampMs (some 100) 200 = 100 := rfl

theorem absent_event_timestamp_uses_observation :
    projectedEventTimestampMs none 200 = 200 := rfl

inductive RequestReplayStage where
  | user
  | compaction
  | modelItems
  deriving DecidableEq, Repr

def completedCompactionReplayStages : List RequestReplayStage :=
  [.user, .compaction, .modelItems]

theorem completed_compaction_replay_matches_runtime_order :
    completedCompactionReplayStages = [.user, .compaction, .modelItems] := rfl

structure ContextUsageObservation where
  cumulativeInput : Nat
  cumulativeOutput : Nat
  latestPrompt : Nat
  latestCompletion : Nat
  modelWindow : Nat
  deriving DecidableEq, Repr

def cumulativeTokens (obs : ContextUsageObservation) : Nat :=
  obs.cumulativeInput + obs.cumulativeOutput

def currentContextTokens (obs : ContextUsageObservation) : Nat :=
  obs.latestPrompt + obs.latestCompletion

def contextRemaining (obs : ContextUsageObservation) : Nat :=
  obs.modelWindow - min obs.modelWindow (currentContextTokens obs)

theorem current_context_uses_latest_call (obs : ContextUsageObservation) :
    currentContextTokens obs = obs.latestPrompt + obs.latestCompletion := rfl

theorem context_remaining_le_window (obs : ContextUsageObservation) :
    contextRemaining obs ≤ obs.modelWindow := by
  simp [contextRemaining]

theorem context_remaining_saturates_at_zero
    (obs : ContextUsageObservation)
    (h : obs.modelWindow ≤ currentContextTokens obs) :
    contextRemaining obs = 0 := by
  simp [contextRemaining, Nat.min_eq_left h]

inductive ContextCompactionEvent where
  | started
  | completed
  deriving DecidableEq, Repr

def initialCompactionEvents : InferenceCallState → List ContextCompactionEvent
  | .queued | .running => [.started]
  | .completed => [.started, .completed]
  | .failed | .cancelled => []

def subsequentCompactionEvents
    (previous current : InferenceCallState) : List ContextCompactionEvent :=
  match previous, current with
  | .queued, .completed | .running, .completed => [.completed]
  | _, _ => []

theorem running_compaction_projects_started :
    initialCompactionEvents .running = [.started] := rfl

theorem queued_compaction_projects_started :
    initialCompactionEvents .queued = [.started] := rfl

theorem completed_first_observation_projects_lifecycle_pair :
    initialCompactionEvents .completed = [.started, .completed] := rfl

theorem running_to_completed_projects_completion :
    subsequentCompactionEvents .running .completed = [.completed] := rfl

theorem running_to_failed_never_claims_completed :
    subsequentCompactionEvents .running .failed = [] := rfl

theorem running_to_cancelled_never_claims_completed :
    subsequentCompactionEvents .running .cancelled = [] := rfl

theorem failed_compaction_never_claims_completed :
    initialCompactionEvents .failed = [] := rfl

theorem cancelled_compaction_never_claims_completed :
    initialCompactionEvents .cancelled = [] := rfl

structure ProjectionObservation where
  requestState : RequestState
  responseStatus : Option ResponseStatus
  localInterruptAcked : Bool
  deriving DecidableEq, Repr

def clientAttemptObservation (obs : ProjectionObservation) : AttemptView :=
  { request :=
      { lifecycleState := obs.requestState
      , isSuperseded := false
      }
  , response := obs.responseStatus.map fun status =>
      { status := status
      , tailEmpty := true
      }
  }

def turnEffectivelyTerminal (obs : ProjectionObservation) : Prop :=
  obs.localInterruptAcked = true ∨ effectivelyTerminal (clientAttemptObservation obs)

instance (obs : ProjectionObservation) : Decidable (turnEffectivelyTerminal obs) := by
  unfold turnEffectivelyTerminal
  infer_instance

def projectObservation (obs : ProjectionObservation) : TurnPhase :=
  if obs.localInterruptAcked then
    .interrupted
  else
    projectClientTurnState (deriveAttempt (clientAttemptObservation obs))

theorem projection_without_local_interrupt
    {obs : ProjectionObservation}
    (h : obs.localInterruptAcked = false) :
    projectObservation obs =
      projectClientTurnState (deriveAttempt (clientAttemptObservation obs)) := by
  simp [projectObservation, h]

theorem local_interrupt_projects_interrupted
    {obs : ProjectionObservation}
    (h : obs.localInterruptAcked = true) :
    projectObservation obs = .interrupted := by
  simp [projectObservation, h]

theorem local_interrupt_never_projects_in_progress
    {obs : ProjectionObservation}
    (h : obs.localInterruptAcked = true) :
    projectObservation obs ≠ .inProgress := by
  rw [local_interrupt_projects_interrupted h]
  intro h_eq
  cases h_eq

theorem codex_turn_terminates_precisely
    (obs : ProjectionObservation) :
    TurnPhase.terminal (projectObservation obs) ↔
      turnEffectivelyTerminal obs := by
  cases h : obs.localInterruptAcked with
  | false =>
      rw [projection_without_local_interrupt h]
      rw [projectClientTurnState_terminal]
      rw [terminal_coherence]
      simp [turnEffectivelyTerminal, h]
  | true =>
      simp [projectObservation, turnEffectivelyTerminal, h, TurnPhase.terminal]

end CodexShim

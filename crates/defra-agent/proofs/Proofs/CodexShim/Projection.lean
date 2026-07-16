import Proofs.Client.Types
import Proofs.CodexShim.TurnLifecycle
import Proofs.InferenceCall.State
import Proofs.Request.Transition

/-!
# Codex Shim Projection

Projection from the core DEFRA request/response model into the lighter
Codex-facing turn lifecycle.

The shim turn phase is not an independent product lifecycle. It is the protocol
view that stock Codex needs while a DEFRA request is running behind it:

* non-terminal DEFRA request states project to `inProgress`;
* terminal DEFRA request states project to a terminal Codex phase;
* response status can advance a non-terminal request observation when response
  replication wins the race;
* local Codex interrupt acknowledgement takes precedence and projects directly
  to `interrupted`, without waiting for the core request row to reach
  `interrupted`.
-/

namespace CodexShim

/-- Coarse rank for monotonicity of the Codex-facing projection. Terminals are
rank-equivalent because Codex needs "not working anymore", not a retry policy
decision, from this adapter layer. -/
def projectedRank : TurnPhase → Nat
  | .notStarted => 0
  | .inProgress => 1
  | .completed => 2
  | .failed => 2
  | .interrupted => 2

/-- Projection of the core `AgentRequest.lifecycle_state` vocabulary into the
Codex app-server turn vocabulary. -/
def projectRequestState : RequestState → TurnPhase
  | .pending => .inProgress
  | .claimed => .inProgress
  | .processing => .inProgress
  | .inputRequired => .inProgress
  | .completed => .completed
  | .failed => .failed
  | .dead => .failed
  | .superseded => .interrupted
  | .interrupted => .interrupted

/-!
## First-class subagent projection

DEFRA's subagent tools and request lifecycle are projected into Codex's
`collabAgentToolCall` vocabulary.  This is deliberately an adapter mapping: it
does not introduce a second subagent lifecycle.
-/

/-- DEFRA tools which have a first-class Codex collaboration equivalent. -/
inductive SubagentControlTool where
  | spawn
  | wait
  | steer
  | cancel
  | other
  deriving DecidableEq, Repr

/-- The Codex collaboration tool variants supported by the pinned app-server
protocol. -/
inductive CollabTool where
  | spawnAgent
  | wait
  | sendInput
  | closeAgent
  deriving DecidableEq, Repr

/-- Project native DEFRA subagent controls into first-class Codex collaboration
items.  Inspection tools (`list_subagents` and `read_subagent`) remain ordinary
MCP calls and enter this model as `other`. -/
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

/-- Whether a subagent control row is ready to be shown to Codex. A reciprocal
link authorizes the native collaboration item. While that link may still
replicate, the item stays deferred; once the enclosing projection is settled,
the durable MCP row is the visibility-preserving fallback. -/
inductive SubagentItemProjection where
  | collab (tool : CollabTool)
  | mcp
  | deferred
  deriving DecidableEq, Repr

def projectSubagentItem
    (tool : SubagentControlTool)
    (hasReciprocalLink projectionSettled : Bool) : SubagentItemProjection :=
  match projectSubagentControl tool with
  | none => .mcp
  | some collab =>
      if hasReciprocalLink then .collab collab
      else if projectionSettled then .mcp
      else .deferred

theorem linked_subagent_control_projects_native
    (tool : SubagentControlTool)
    (collab : CollabTool)
    (h : projectSubagentControl tool = some collab) :
    projectSubagentItem tool true false = .collab collab := by
  simp [projectSubagentItem, h]

theorem unresolved_subagent_control_defers_while_open
    (tool : SubagentControlTool)
    (collab : CollabTool)
    (h : projectSubagentControl tool = some collab) :
    projectSubagentItem tool false false = .deferred := by
  simp [projectSubagentItem, h]

theorem settled_unresolved_subagent_control_stays_visible
    (tool : SubagentControlTool)
    (collab : CollabTool)
    (h : projectSubagentControl tool = some collab) :
    projectSubagentItem tool false true = .mcp := by
  simp [projectSubagentItem, h]

/-- Codex's per-agent status vocabulary.  `shutdown` and `notFound` are local
app-server bookkeeping states, so no core request lifecycle maps to them. -/
inductive CollabAgentPhase where
  | pendingInit
  | running
  | completed
  | errored
  | interrupted
  deriving DecidableEq, Repr

/-- A child agent is terminal exactly when its DEFRA request is terminal. -/
def CollabAgentPhase.terminal : CollabAgentPhase → Prop
  | .completed | .errored | .interrupted => True
  | .pendingInit | .running => False

/-- Project the authoritative child request lifecycle into Codex's agent status. -/
def projectSubagentState : RequestState → CollabAgentPhase
  | .pending => .pendingInit
  | .claimed | .processing | .inputRequired => .running
  | .completed => .completed
  | .failed | .dead => .errored
  | .superseded | .interrupted => .interrupted

theorem subagent_status_terminal_precisely
    (state : RequestState) :
    CollabAgentPhase.terminal (projectSubagentState state) ↔
      isTerminal state := by
  cases state <;>
    simp [ projectSubagentState
         , CollabAgentPhase.terminal
         , HasTerminal.isTerminal
         , RequestState.instHasTerminal
         ]

/-- The equality key used by the live adapter must carry the projected child
state and failure message as well as the parent tool status. Otherwise a child
transition cannot refresh Codex's `agentsStates` after the tool item completes. -/
structure CollabPresentationFingerprint where
  childPhase : CollabAgentPhase
  failureMessage : Option String
  deriving DecidableEq, Repr

def collabPresentationFingerprint
    (latestChildState : RequestState)
    (failureMessage : Option String) : CollabPresentationFingerprint :=
  { childPhase := projectSubagentState latestChildState
  , failureMessage := failureMessage
  }

theorem collab_fingerprint_includes_latest_child_state
    (state : RequestState)
    (failureMessage : Option String) :
    (collabPresentationFingerprint state failureMessage).childPhase =
      projectSubagentState state := rfl

/-- Authorized child threads remain durable snapshots until the client loads
them. Loading an authorized child upgrades the projection to the live event
stream; authorization is required in both modes. -/
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

/-- The bridge between a parent tool call and a child request.  Codex thread IDs
identify sessions, not requests, so `childSessionId` is the only sound receiver
identifier for TUI navigation. -/
structure SubagentThreadLink where
  childRequestId : String
  childSessionId : String
  deriving DecidableEq, Repr

def receiverThreadId (link : SubagentThreadLink) : String :=
  link.childSessionId

theorem receiver_thread_is_child_session (link : SubagentThreadLink) :
    receiverThreadId link = link.childSessionId := rfl

/-!
### Subagent presentation metadata and discovery

The runtime remains authoritative for optional presentation metadata.  The
adapter preserves values which DEFRA owns and leaves unavailable values absent;
it never substitutes Codex defaults.  Authorized DEFRA children are ordinary
spawned subagent threads in the Codex source vocabulary.
-/

/-- Runtime-owned metadata carried by a native collaboration item. -/
structure CollabPresentationMetadata where
  model : Option String
  reasoningEffort : Option String
  deriving DecidableEq, Repr

/-- Projection is intentionally identity: the shim formats runtime facts but
does not create an independent configuration source. -/
def projectCollabPresentationMetadata
    (metadata : CollabPresentationMetadata) : CollabPresentationMetadata :=
  metadata

theorem collab_model_is_runtime_model (metadata : CollabPresentationMetadata) :
    (projectCollabPresentationMetadata metadata).model = metadata.model := rfl

theorem absent_runtime_reasoning_effort_stays_absent
    (model : Option String) :
    (projectCollabPresentationMetadata
      { model := model, reasoningEffort := none }).reasoningEffort = none := rfl

/-- Source filters relevant to a DEFRA child created by `spawn_subagent`. -/
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

/-- The pinned Codex `Thread` carries spawn ancestry inside
`source.subAgent.thread_spawn`; there is no top-level `parentThreadId` field. -/
structure SubagentThreadParentProjection where
  nativeSourceParent : Option String
  legacyTopLevelParent : Option String
  deriving DecidableEq, Repr

def projectSubagentThreadParent (parentThreadId : String) :
    SubagentThreadParentProjection :=
  { nativeSourceParent := some parentThreadId
  , legacyTopLevelParent := none
  }

theorem subagent_parent_uses_native_source (parentThreadId : String) :
    (projectSubagentThreadParent parentThreadId).nativeSourceParent =
      some parentThreadId := rfl

theorem subagent_parent_omits_legacy_top_level (parentThreadId : String) :
    (projectSubagentThreadParent parentThreadId).legacyTopLevelParent = none := rfl

/-!
## Native reasoning presentation

DEFRA owns two views of reasoning: the bounded live `AgentResponse.reasoning`
preview and the durable `AgentMessage.reasoning` copy materialized before the
live tail is cleared.  The shim projects both through Codex's raw reasoning
channel. It must not relabel provider reasoning text as a summary merely to
make a client render it by default.

`liveDelta` is the append-only text recovered by the adapter's bounded-tail
cursor. A primed cursor on child-thread resume therefore supplies `none` until
new reasoning arrives. At terminal observation, durable text is used to create
the lifecycle when replication skipped every live observation and is always
the authoritative completed-item payload.
-/

/-- Codex item notifications emitted for native reasoning text. The event type
deliberately has only a raw-text delta constructor, so the adapter cannot
accidentally promote raw reasoning to a summary. -/
inductive ReasoningProjectionEvent where
  | started
  | rawTextDelta (text : String)
  | completed
  deriving DecidableEq, Repr

/-- Facts available at one reasoning projection observation. -/
structure ReasoningProjectionObservation where
  itemOpen : Bool
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

/-- Text to expose at this observation. Live cursor output wins. A durable
terminal value is streamed only when no item was already opened or primed. -/
def reasoningTextForObservation
    (obs : ReasoningProjectionObservation) : Option String :=
  match nonemptyReasoningText obs.liveDelta with
  | some text => some text
  | none =>
      if obs.terminal && !obs.itemOpen && !obs.cursorPrimed then
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
    if obs.terminal && openAfter then [.completed] else []
  startEvents ++ deltaEvents ++ completionEvents

/-- Completed-item content comes from the durable materialized message, never
from the bounded live preview, when that durable value exists. -/
def completedReasoningText
    (obs : ReasoningProjectionObservation) : Option String :=
  if obs.terminal then preferReasoningText obs.durableText obs.streamedText else none

theorem first_live_reasoning_projects_raw_lifecycle :
    reasoningProjectionEvents
      { itemOpen := false
      , cursorPrimed := false
      , streamedText := none
      , liveDelta := some "inspect"
      , durableText := none
      , terminal := false } =
      [.started, .rawTextDelta "inspect"] := rfl

theorem appended_live_reasoning_projects_only_delta :
    reasoningProjectionEvents
      { itemOpen := true
      , cursorPrimed := false
      , streamedText := some "inspect"
      , liveDelta := some " then test"
      , durableText := none
      , terminal := false } =
      [.rawTextDelta " then test"] := rfl

theorem primed_resume_without_new_reasoning_replays_nothing :
    reasoningProjectionEvents
      { itemOpen := true
      , cursorPrimed := true
      , streamedText := some "already visible"
      , liveDelta := none
      , durableText := none
      , terminal := false } = [] := rfl

theorem terminal_open_reasoning_completes_without_replay :
    reasoningProjectionEvents
      { itemOpen := true
      , cursorPrimed := false
      , streamedText := some "inspect then test"
      , liveDelta := none
      , durableText := some "inspect then test"
      , terminal := true } = [.completed] := rfl

theorem terminal_durable_first_observation_projects_lifecycle :
    reasoningProjectionEvents
      { itemOpen := false
      , cursorPrimed := false
      , streamedText := none
      , liveDelta := none
      , durableText := some "inspect then test"
      , terminal := true } =
      [.started, .rawTextDelta "inspect then test", .completed] := rfl

theorem absent_reasoning_projects_nothing :
    reasoningProjectionEvents
      { itemOpen := false
      , cursorPrimed := false
      , streamedText := none
      , liveDelta := none
      , durableText := none
      , terminal := true } = [] := rfl

theorem terminal_completed_item_uses_durable_reasoning :
    completedReasoningText
      { itemOpen := true
      , cursorPrimed := false
      , streamedText := some "bounded preview"
      , liveDelta := none
      , durableText := some "durable reasoning"
      , terminal := true } = some "durable reasoning" := rfl

theorem terminal_without_durable_reasoning_keeps_streamed_text :
    completedReasoningText
      { itemOpen := true
      , cursorPrimed := false
      , streamedText := some "streamed reasoning"
      , liveDelta := none
      , durableText := none
      , terminal := true } = some "streamed reasoning" := rfl

theorem terminal_durable_suffix_projects_before_completion :
    reasoningProjectionEvents
      { itemOpen := true
      , cursorPrimed := false
      , streamedText := some "inspect then test"
      , liveDelta := some "; durable suffix"
      , durableText := some "inspect then test; durable suffix"
      , terminal := true } =
      [.rawTextDelta "; durable suffix", .completed] := rfl

/-!
## Runtime metadata hydration

The runtime documents remain authoritative for thread liveness, behavior/model
binding, tool identity and diagnostics, and timestamps. These functions define
the loss-minimizing choices the stateless shim makes when Codex has fewer
fields than DEFRA.
-/

inductive ThreadPresentationStatus where
  | active
  | idle
  | systemError
  deriving DecidableEq, Repr

def projectThreadStatus
    (requestState : Option RequestState)
    (conversationStatus : String) : ThreadPresentationStatus :=
  match requestState with
  | some .pending | some .claimed | some .processing | some .inputRequired => .active
  | some .failed | some .dead => .systemError
  | some .completed | some .superseded | some .interrupted => .idle
  | none => if conversationStatus = "error" then .systemError else .idle

def projectionBehaviorId (rootBehaviorId : String)
    (threadBehaviorId : Option String) : String :=
  match nonemptyReasoningText threadBehaviorId with
  | some behaviorId => behaviorId
  | none => rootBehaviorId

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
          match nonemptyReasoningText failureClass with
          | some value => some value
          | none => nonemptyReasoningText result

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
    projectThreadStatus (some .processing) "completed" = .active := rfl

theorem failed_request_projects_system_error_thread :
    projectThreadStatus (some .failed) "active" = .systemError := rfl

theorem completed_request_projects_idle_thread :
    projectThreadStatus (some .completed) "error" = .idle := rfl

theorem missing_request_error_conversation_projects_system_error :
    projectThreadStatus none "error" = .systemError := rfl

theorem missing_request_active_conversation_is_quiescent :
    projectThreadStatus none "active" = .idle := rfl

theorem child_behavior_overrides_root_for_response_metadata :
    projectionBehaviorId "root" (some "child") = "child" := rfl

theorem absent_child_behavior_keeps_root_response_metadata :
    projectionBehaviorId "root" none = "root" := rfl

theorem selected_tool_identity_overrides_model_facing_name :
    projectedToolIdentity "defra" (some "service-a") = "service-a" := rfl

theorem absent_selected_tool_identity_keeps_fallback :
    projectedToolIdentity "defra" none = "defra" := rfl

theorem denial_diagnostic_has_priority :
    projectedToolFailure
      (some "policy denied") (some "interrupted") (some "policyDenied")
      (some "generic result") = some "policy denied" := rfl

theorem cancellation_diagnostic_precedes_failure_class :
    projectedToolFailure none (some "deadline") (some "timedOut") none =
      some "deadline" := rfl

theorem failure_class_precedes_result_fallback :
    projectedToolFailure none none (some "argumentInvalid") (some "generic result") =
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

/-- Automatic compaction runs after the request is accepted and before the
owned inference/tool loop, so replay places its item between the user request
and model-derived items. -/
inductive RequestReplayStage where
  | user
  | compaction
  | modelItems
  deriving DecidableEq, Repr

def completedCompactionReplayStages : List RequestReplayStage :=
  [.user, .compaction, .modelItems]

theorem completed_compaction_replay_matches_runtime_order :
    completedCompactionReplayStages = [.user, .compaction, .modelItems] := rfl

/-!
## Context and compaction presentation

Codex distinguishes cumulative token accounting from the tokens occupying the
current model context.  DEFRA persists both views in `InferenceCall`: the sum of
all terminal calls is cumulative accounting, while the newest inference call is
the context observation.  The effective inference-profile window supplies the
capacity.

Compaction presentation is likewise derived from the persisted compaction
`InferenceCall`; the shim does not introduce a second compaction lifecycle.
-/

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

/-- Codex item notifications emitted for a persisted compaction call. -/
inductive ContextCompactionEvent where
  | started
  | completed
  deriving DecidableEq, Repr

/-- Events required when the first observation of a compaction call arrives.
A completed row can win the replication race, so it emits the pair needed for
a well-formed Codex item lifecycle. Failed/cancelled rows never claim success. -/
def initialCompactionEvents : InferenceCallState → List ContextCompactionEvent
  | .queued | .running => [.started]
  | .completed => [.started, .completed]
  | .failed | .cancelled => []

/-- Events required after a nonterminal compaction call was already observed.
The pinned protocol has no failed-item notification. Failed/cancelled calls
therefore emit no success event; a client which renders `started` must clear the
in-progress presentation when the enclosing turn terminates. -/
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

/-- Observation available to the shim while streaming a Codex turn. -/
structure ProjectionObservation where
  requestState : RequestState
  responseStatus : Option ResponseStatus
  localInterruptAcked : Bool
  deriving DecidableEq, Repr

/-- Response statuses that make the Codex-facing turn effectively terminal even
when the request row has not replicated to a terminal lifecycle state yet. -/
def responseStatusTerminal : Option ResponseStatus → Prop
  | some .complete => True
  | some .error => True
  | some .streaming => False
  | none => False

/-- Terminality observed at the Codex shim boundary.

The local interrupt acknowledgement is part of the effective terminal condition:
the shim must clear the active Codex turn as soon as it has accepted the local
interrupt, even while the core request row is settling asynchronously. -/
def turnEffectivelyTerminal (obs : ProjectionObservation) : Prop :=
  isTerminal obs.requestState ∨
  obs.localInterruptAcked = true ∨
  responseStatusTerminal obs.responseStatus

/-- Project a live DEFRA observation to the Codex-facing turn phase.

For non-terminal request states, the response may be newer than the request row
under replication lag, so complete/error responses can advance the projection.
For terminal request states, the request lifecycle wins. -/
def projectObservation (obs : ProjectionObservation) : TurnPhase :=
  if obs.localInterruptAcked then
    .interrupted
  else
    match obs.requestState with
    | .pending | .claimed | .processing | .inputRequired =>
      match obs.responseStatus with
      | some .complete => .completed
      | some .error => .failed
      | some .streaming => .inProgress
      | none => .inProgress
    | .completed => .completed
    | .failed => .failed
    | .dead => .failed
    | .superseded => .interrupted
    | .interrupted => .interrupted

theorem project_pending_is_in_progress :
    projectRequestState .pending = .inProgress := rfl

theorem project_claimed_is_in_progress :
    projectRequestState .claimed = .inProgress := rfl

theorem project_processing_is_in_progress :
    projectRequestState .processing = .inProgress := rfl

theorem project_completed_is_completed :
    projectRequestState .completed = .completed := rfl

theorem project_failed_is_failed :
    projectRequestState .failed = .failed := rfl

theorem project_dead_is_failed :
    projectRequestState .dead = .failed := rfl

theorem project_superseded_is_interrupted :
    projectRequestState .superseded = .interrupted := rfl

theorem project_interrupted_is_interrupted :
    projectRequestState .interrupted = .interrupted := rfl

theorem nonterminal_without_response_projects_in_progress
    {s : RequestState}
    (h : s = .pending ∨ s = .claimed ∨ s = .processing ∨ s = .inputRequired) :
    projectObservation
      { requestState := s, responseStatus := none, localInterruptAcked := false } =
        .inProgress := by
  rcases h with h | h | h | h <;> subst s <;> rfl

theorem response_complete_advances_nonterminal_to_completed
    {s : RequestState}
    (h : s = .pending ∨ s = .claimed ∨ s = .processing ∨ s = .inputRequired) :
    projectObservation
      { requestState := s
      , responseStatus := some .complete
      , localInterruptAcked := false } = .completed := by
  rcases h with h | h | h | h <;> subst s <;> rfl

theorem response_error_advances_nonterminal_to_failed
    {s : RequestState}
    (h : s = .pending ∨ s = .claimed ∨ s = .processing ∨ s = .inputRequired) :
    projectObservation
      { requestState := s
      , responseStatus := some .error
      , localInterruptAcked := false } = .failed := by
  rcases h with h | h | h | h <;> subst s <;> rfl

theorem terminal_request_overrides_response
    {s : RequestState}
    {resp : Option ResponseStatus}
    (h : s = .completed ∨ s = .failed ∨ s = .dead ∨
         s = .superseded ∨ s = .interrupted) :
    projectObservation
      { requestState := s
      , responseStatus := resp
      , localInterruptAcked := false } = projectRequestState s := by
  rcases h with h | h | h | h | h <;> subst s <;> cases resp <;> rfl

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

theorem terminal_request_projects_terminal
    {s : RequestState}
    (h : s = .completed ∨ s = .failed ∨ s = .superseded ∨
         s = .dead ∨ s = .interrupted) :
    TurnPhase.terminal (projectRequestState s) := by
  rcases h with h | h | h | h | h <;> subst s <;> trivial

/-- Terminal coherence for the Codex shim projection.

The projected turn is terminal exactly when the request is terminal, the shim has
locally acknowledged an interrupt, or the response has already reached a terminal
status under replication lag. -/
theorem codex_turn_terminates_precisely
    (obs : ProjectionObservation) :
    TurnPhase.terminal (projectObservation obs) ↔
      turnEffectivelyTerminal obs := by
  obtain ⟨requestState, responseStatus, localInterruptAcked⟩ := obs
  cases localInterruptAcked <;> cases requestState
  all_goals
    cases responseStatus with
    | none =>
        simp [ projectObservation
             , turnEffectivelyTerminal
             , responseStatusTerminal
             , TurnPhase.terminal
             , HasTerminal.isTerminal
             , RequestState.instHasTerminal
             ]
    | some status =>
        cases status <;>
          simp [ projectObservation
               , turnEffectivelyTerminal
               , responseStatusTerminal
               , TurnPhase.terminal
               , HasTerminal.isTerminal
               , RequestState.instHasTerminal
               ]

/-- Core request transitions never move the Codex projection backwards. -/
theorem request_transition_projection_monotonic
    {pre post : RequestContext}
    (h : RequestContext.Transition pre post) :
    projectedRank (projectRequestState post.state) ≥
      projectedRank (projectRequestState pre.state) := by
  cases h with
  | claim h_state _ _ h_post =>
      subst h_post
      simp [projectRequestState, projectedRank, h_state]
  | dedup_lose h_state _ h_post =>
      subst h_post
      simp [projectRequestState, projectedRank, h_state]
  | begin_inference h_state _ h_post =>
      subst h_post
      simp [projectRequestState, projectedRank, h_state]
  | advance h_state _ h_post =>
      subst h_post
      simp [projectRequestState, projectedRank, h_state]
  | finish h_state _ h_post =>
      subst h_post
      simp [projectRequestState, projectedRank, h_state]
  | fail h_state _ h_post =>
      subst h_post
      simp [projectRequestState, projectedRank, h_state]
  | fail_before_stream h_state _ h_post =>
      subst h_post
      simp [projectRequestState, projectedRank, h_state]
  | expire h_state _ _ _ h_post =>
      subst h_post
      simp [projectRequestState, projectedRank, h_state]
  | interrupt_before_claim h_state _ _ h_post =>
      subst h_post
      simp [projectRequestState, projectedRank, h_state]
  | interrupt_claimed h_state _ _ h_post =>
      subst h_post
      simp [projectRequestState, projectedRank, h_state]
  | interrupt_processing h_state _ _ h_post =>
      subst h_post
      simp [projectRequestState, projectedRank, h_state]

end CodexShim

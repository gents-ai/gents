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

/-- Events required after a nonterminal compaction call was already observed. -/
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

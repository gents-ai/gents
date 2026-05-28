import Proofs.Client.Types
import Proofs.CodexShim.TurnLifecycle
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

/-- Observation available to the shim while streaming a Codex turn. -/
structure ProjectionObservation where
  requestState : RequestState
  responseStatus : Option ResponseStatus
  localInterruptAcked : Bool
  deriving DecidableEq, Repr

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

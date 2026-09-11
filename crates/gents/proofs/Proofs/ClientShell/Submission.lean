import Proofs.ClientShell.Types

structure SubmitContext where
  clientAvailable   : Bool
  composerNonEmpty  : Bool
  requestedBehavior : Option BehaviorId
  deriving Repr

def behaviorMismatch
    (store : LocalStore) (sid : SessionId)
    (requested : Option BehaviorId) : Bool :=
  match requested, (store.find sid).bind (·.behaviorId) with
  | some r, some e => decide (r ≠ e)
  | _, _           => false

inductive SendBlockedReason where
  | clientOffline
  | agentNotSelected
  | composerEmpty
  | mutationInFlight
  | awaitingObservation
  | awaitingTurnTerminality (turn : ClientTurnState)
  | sessionBehaviorMismatch
  | sessionAbsent
  | inconsistentObservation
  | workflowBlocked
  deriving DecidableEq, Repr

inductive SendDecision where
  | ready
  | blocked (reason : SendBlockedReason)
  deriving DecidableEq, Repr

def projectSendDecision
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext) : SendDecision :=
  if ¬ ctx.clientAvailable then .blocked .clientOffline
  else if s.selection.agent.isNone then .blocked .agentNotSelected
  else if ¬ ctx.composerNonEmpty then .blocked .composerEmpty
  else match s.workflow with
    | .submitting _ _ => .blocked .mutationInFlight
    | .awaiting _ _                 => .blocked .awaitingObservation
    | .blocked _                    => .blocked .workflowBlocked
    | .idle =>
      match s.selection.session with
      | none     => .ready
      | some sid =>
        match store.find sid with
        | none     =>
          .blocked .sessionAbsent
        | some obs =>
          if behaviorMismatch store sid ctx.requestedBehavior then
            .blocked .sessionBehaviorMismatch
          else
            match obs.latestObservedRequest, obs.latestTurn with
            | none,   none   => .ready
            | some _, some t =>
              if t.isTerminal then .ready
              else .blocked (.awaitingTurnTerminality t)
            | _,      _      => .blocked .inconsistentObservation

def canSubmit (s : ShellState) (store : LocalStore) (ctx : SubmitContext) : Bool :=
  projectSendDecision s store ctx == .ready

/-- Submission and its diagnostic projection share exactly one decision owner. -/
theorem canSubmit_iff_ready (s : ShellState) (store : LocalStore) (ctx : SubmitContext) :
    canSubmit s store ctx = true ↔ projectSendDecision s store ctx = .ready := by
  simp [canSubmit]

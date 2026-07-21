/-!
# Codex Shim Turn Lifecycle

Small Codex-facing lifecycle for a single app-server turn.

This deliberately models the adapter boundary, not the core GENTS request
lifecycle. The important property is that a Codex interrupt is locally terminal:
the shim must emit a terminal turn notification and clear its active-turn slot
without waiting for the asynchronous GENTS request interrupt to settle.
-/

namespace CodexShim

inductive TurnPhase where
  | notStarted
  | inProgress
  | completed
  | failed
  | interrupted
  deriving DecidableEq, Repr

namespace TurnPhase

def toProtocol : TurnPhase → String
  | .notStarted => "notStarted"
  | .inProgress => "inProgress"
  | .completed => "completed"
  | .failed => "failed"
  | .interrupted => "interrupted"

def terminal : TurnPhase → Prop
  | .completed => True
  | .failed => True
  | .interrupted => True
  | .notStarted => False
  | .inProgress => False

/-- Lexicographic rank used for monotonic turn-lifecycle checks.
Terminals are rank-equivalent because the adapter only needs to know that the
turn is no longer active. -/
def lexOrd : TurnPhase → Nat
  | .notStarted => 0
  | .inProgress => 1
  | .completed => 2
  | .failed => 2
  | .interrupted => 2

end TurnPhase

inductive TurnAction where
  | start
  | complete
  | fail
  | interrupt
  deriving DecidableEq, Repr

inductive TurnTransition : TurnPhase → TurnPhase → TurnAction → Prop where
  | start :
      TurnTransition .notStarted .inProgress .start
  | complete :
      TurnTransition .inProgress .completed .complete
  | fail :
      TurnTransition .inProgress .failed .fail
  | interrupt :
      TurnTransition .inProgress .interrupted .interrupt

def turnStep? (pre : TurnPhase) : TurnAction → Option TurnPhase
  | .start =>
      if pre = .notStarted then some .inProgress else none
  | .complete =>
      if pre = .inProgress then some .completed else none
  | .fail =>
      if pre = .inProgress then some .failed else none
  | .interrupt =>
      if pre = .inProgress then some .interrupted else none

theorem interrupt_from_in_progress_is_terminal
    {post : TurnPhase}
    (h : TurnTransition .inProgress post .interrupt) :
    post = .interrupted := by
  cases h
  rfl

theorem interrupt_cannot_stutter :
    ¬ TurnTransition .inProgress .inProgress .interrupt := by
  intro h
  cases h

theorem interrupt_step_is_terminal :
    turnStep? .inProgress .interrupt = some .interrupted := by
  rfl

theorem interrupted_is_terminal :
    TurnPhase.terminal .interrupted := by
  trivial

/-- Valid Codex-facing turn lifecycle transitions never move backward. -/
theorem turn_lifecycle_never_regresses
    {pre post : TurnPhase}
    {action : TurnAction}
    (h : TurnTransition pre post action) :
    TurnPhase.lexOrd post ≥ TurnPhase.lexOrd pre := by
  cases h <;> simp [TurnPhase.lexOrd]

end CodexShim

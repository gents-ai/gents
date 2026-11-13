/-! The pairing reconciler's supervisor: every sweep is raced against shutdown,
and a sweep that fails waits for the reconciler's next interval tick instead
of ending the reconciler. -/

namespace PairingReconcile.Supervisor

/-- Premise (embedded transport): each remote admin call is bounded by the
embedded admin timeout, ten seconds (`DEFAULT_EMBEDDED_ADMIN_TIMEOUT`). -/
def perAdminCallTimeoutMs : Nat := 10000

/-- What a sweep raced against shutdown shows. -/
structure RaceOutcome where
  cancellationObservedInsideSweep : Bool
  currentAdminFutureDropped : Bool
  remainingPeersSkipped : Bool
  shutdownJoinBounded : Bool
  deriving DecidableEq, Repr

/-- Decision: shutdown wins the race against an in-flight sweep. A sweep
cancelled while it waits on an admin call drops that call and skips the peers
it has not reached; a sweep that is not cancelled finishes every peer. Either
way the supervisor's join returns without waiting out an admin call. -/
def race (cancelledDuringAdminWait : Bool) : RaceOutcome :=
  { cancellationObservedInsideSweep := cancelledDuringAdminWait
    currentAdminFutureDropped := cancelledDuringAdminWait
    remainingPeersSkipped := cancelledDuringAdminWait
    shutdownJoinBounded := true }

/-- When a failed sweep is retried. -/
inductive RetryTrigger where
  | immediateFirstIntervalTick
  | laterIntervalTick
  deriving DecidableEq, Repr

def RetryTrigger.toWire : RetryTrigger → String
  | .immediateFirstIntervalTick => "immediateFirstIntervalTick"
  | .laterIntervalTick => "laterIntervalTick"

structure RetryOutcome where
  failureTerminal : Bool
  retryTrigger : Option RetryTrigger
  cancellationPrioritized : Bool
  convergenceRetried : Bool
  enumerationAttempts : Nat
  deriving DecidableEq, Repr

/-- Decision: a failed top-level sweep is never terminal. The reconciler runs
its first sweep at startup and every later one on an interval tick whose first
tick is ready at once, so the first retry costs no wait and later retries wait
an interval each. Shutdown still wins every race. -/
def retry (initialFailures : Nat) : RetryOutcome :=
  { failureTerminal := false
    retryTrigger :=
      if initialFailures = 0 then none
      else if initialFailures = 1 then some .immediateFirstIntervalTick
      else some .laterIntervalTick
    cancellationPrioritized := true
    convergenceRetried := initialFailures != 0
    enumerationAttempts := initialFailures + 1 }

theorem a_failed_sweep_never_ends_the_reconciler (n : Nat) :
    (retry n).failureTerminal = false := rfl

theorem shutdown_join_is_always_bounded (cancelled : Bool) :
    (race cancelled).shutdownJoinBounded = true := rfl

end PairingReconcile.Supervisor

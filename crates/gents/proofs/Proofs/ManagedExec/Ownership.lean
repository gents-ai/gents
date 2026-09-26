import Proofs.Basic

/-!
# Host process ownership and verified stop

A managed child runs as the leader of its own session and process group
(`setsid`), so host service managers do not reap it with the runtime. The
runtime's volatile worker is its only in-memory owner; a runtime crash loses
that owner while the process keeps running.

Ownership after restart is therefore a host observation over a durable record
kept beside the runtime's exclusively locked store: the leader pid, its process
group, and an OS start identity. A pid alone is not ownership evidence, since
the OS reuses pids. A group whose recorded leader is gone is not evidence
either: once the original group empties, its id can be reused. Neither case
licenses a signal, and neither may be reported as stopped.

`StopOutcome` is the owner's verdict after it signals a proven-owned group
and observes it again. Only `stopped` may be reported as a cancellation; the
reply never claims a stop the owner did not observe.
-/

namespace ManagedExec

/-- What the host owner observes about one background execution. A live
    in-runtime worker, or a recorded leader whose start identity matches, is
    proven ownership. `running` means a non-zombie member remains; zombies are
    exited processes awaiting reap. -/
inductive ProcessObservation where
  | running
  | exited
  | unowned
  deriving DecidableEq, Repr

namespace ProcessObservation

def toContract : ProcessObservation → String
  | .running => "running"
  | .exited => "exited"
  | .unowned => "unowned"

def all : List ProcessObservation := [.running, .exited, .unowned]

theorem all_complete (o : ProcessObservation) : o ∈ all := by
  cases o <;> simp [all]

end ProcessObservation

/-- Signals are admissible only against a proven-owned, still-running group. -/
def mayTerminate : ProcessObservation → Bool
  | .running => true
  | _ => false

theorem mayTerminate_iff_running (o : ProcessObservation) :
    mayTerminate o = true ↔ o = .running := by
  cases o <;> simp [mayTerminate]

/-- The owner's verdict from the observation before its signal and the
    observation after it. `after` is only consulted for a group the owner
    signalled; a group that was not running needs no signal. -/
inductive StopOutcome where
  | stopped
  | alreadyExited
  | stillRunning
  | notOwned
  deriving DecidableEq, Repr

namespace StopOutcome

def toContract : StopOutcome → String
  | .stopped => "stopped"
  | .alreadyExited => "alreadyExited"
  | .stillRunning => "stillRunning"
  | .notOwned => "notOwned"

def all : List StopOutcome := [.stopped, .alreadyExited, .stillRunning, .notOwned]

theorem all_complete (o : StopOutcome) : o ∈ all := by
  cases o <;> simp [all]

end StopOutcome

def stopOutcome (before after : ProcessObservation) : StopOutcome :=
  match before, after with
  | .running, .exited => .stopped
  | .running, _ => .stillRunning
  | .exited, _ => .alreadyExited
  | .unowned, _ => .notOwned

/-- `stopped` requires both a proven-owned running group before the signal and
    an observed exit after it. -/
theorem stopped_requires_owned_running_then_exited
    (before after : ProcessObservation)
    (h : stopOutcome before after = .stopped) :
    before = .running ∧ after = .exited := by
  cases before <;> cases after <;> simp_all [stopOutcome]

/-- An unowned observation never becomes a stop, whatever is observed later. -/
theorem unowned_never_stopped (after : ProcessObservation) :
    stopOutcome .unowned after = .notOwned := by
  cases after <;> rfl

/-- Reply to an explicit process cancellation. `lost` settles the execution
    without a stop claim: it was not owned, or it had already exited while no
    owner observed its result. `unverified` means the owner signalled but still
    observes a running member. -/
inductive CancelReply where
  | cancelled
  | lost
  | unverified
  deriving DecidableEq, Repr

namespace CancelReply

def toContract : CancelReply → String
  | .cancelled => "cancelled"
  | .lost => "lost"
  | .unverified => "unverified"

def all : List CancelReply := [.cancelled, .lost, .unverified]

theorem all_complete (r : CancelReply) : r ∈ all := by
  cases r <;> simp [all]

end CancelReply

def cancelReply : StopOutcome → CancelReply
  | .stopped => .cancelled
  | .alreadyExited => .lost
  | .notOwned => .lost
  | .stillRunning => .unverified

/-- No false cancellation: `cancelled` is reported exactly for an observed stop
    of a proven-owned running group. -/
theorem cancelReply_cancelled_iff_stopped (o : StopOutcome) :
    cancelReply o = .cancelled ↔ o = .stopped := by
  cases o <;> simp [cancelReply]

theorem cancelled_reply_observed_stop
    (before after : ProcessObservation)
    (h : cancelReply (stopOutcome before after) = .cancelled) :
    before = .running ∧ after = .exited :=
  stopped_requires_owned_running_then_exited before after
    ((cancelReply_cancelled_iff_stopped _).mp h)

end ManagedExec

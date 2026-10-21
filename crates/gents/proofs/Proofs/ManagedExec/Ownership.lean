import Proofs.Basic

/-!
# Host process ownership and verified stop

A pid alone is not ownership evidence (the OS reuses pids), and neither is a
group whose recorded leader is gone (group ids can be reused). Neither case
licenses a signal or may be reported as stopped: only `stopped` — an observed
exit of a proven-owned running group — may be reported as a cancellation.
-/

namespace ManagedExec

/-- A proven-owned running group, or a recorded leader whose start identity
matches; `running` means a non-zombie member remains. -/
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

def mayTerminate : ProcessObservation → Bool
  | .running => true
  | _ => false

/-- Keep process ownership evidence until the group is positively observed
exited. An unknown leader/group observation is not proof that descendants are
gone. -/
def retainProcessRecord : ProcessObservation → Bool
  | .exited => false
  | .running | .unowned => true

theorem retainProcessRecord_iff_not_exited (o : ProcessObservation) :
    retainProcessRecord o = true ↔ o ≠ .exited := by
  cases o <;> simp [retainProcessRecord]

theorem mayTerminate_iff_running (o : ProcessObservation) :
    mayTerminate o = true ↔ o = .running := by
  cases o <;> simp [mayTerminate]

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

theorem stopped_requires_owned_running_then_exited
    (before after : ProcessObservation)
    (h : stopOutcome before after = .stopped) :
    before = .running ∧ after = .exited := by
  cases before <;> cases after <;> simp_all [stopOutcome]

theorem unowned_never_stopped (after : ProcessObservation) :
    stopOutcome .unowned after = .notOwned := by
  cases after <;> rfl

/-- `lost` settles without a stop claim; `unverified` means the owner signalled
but still observes a running member. -/
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

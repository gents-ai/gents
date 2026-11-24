import Proofs.Documents.ScheduleCadence
import Proofs.Conformance.Family

namespace Conformance.ScheduleCadence

open Lean _root_.ScheduleCadence

inductive CadenceKind where
  | interval
  | everyMinute
  deriving ToJson

/-- `everyMinute` is the cron expression `* * * * *` in UTC with the
latest-only missed-run policy. External premise: its occurrences are the minute
boundaries, so the first strictly after `t` is `(t / 60 + 1) * 60`. -/
def CadenceKind.next (period : Nat) : CadenceKind → Nat → Nat
  | .interval => (· + period)
  | .everyMinute => fun t => (t / 60 + 1) * 60

def CadenceKind.policy : CadenceKind → MissedRunPolicy
  | .interval => .catchUp
  | .everyMinute => .latestOnly

structure AdvanceInput where
  cadence : CadenceKind
  intervalSecs : Nat
  due : Nat
  now : Nat
  deriving ToJson

@[conformance_family]
def advanceCursor : Family AdvanceInput Nat where
  inputs := [CadenceKind.interval, .everyMinute].flatMap fun cadence =>
    [(600, 600), (600, 659), (600, 905), (600, 3600)].map fun (due, now) =>
      { cadence, intervalSecs := 60, due, now }
  model := fun i => advance (i.cadence.next i.intervalSecs) i.cadence.policy i.due i.now

end Conformance.ScheduleCadence

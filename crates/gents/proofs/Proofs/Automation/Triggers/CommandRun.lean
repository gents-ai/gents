import Proofs.Automation.Triggers.Types

/-! A command Task's fire runs a host command on the Node, with no Agent and
no request: the trigger engine decides from the Trigger's concurrency mode and
whether one of its runs is still going what that fire does. -/

namespace Triggers.CommandRun

inductive Admission where
  /-- Launch the command. -/
  | start
  /-- A run is going and the Trigger is `serial`: this fire is skipped. -/
  | skipBusy
  /-- A run is going and the Trigger is `latest_only`: interrupt it, then launch. -/
  | supersedeThenStart
  /-- `queued_serial` persists every fire for its Agent; a command has no queue. -/
  | refuseQueued
  deriving DecidableEq, Repr

/-- External premise: the processes a fire launches live only as long as the
runtime that launched them, so whether a run is going is the runtime's own
in-memory knowledge, not a document. -/
def admit (concurrency : ConcurrencyMode) (running : Bool) : Admission :=
  match concurrency, running with
  | .queuedSerial, _ => .refuseQueued
  | .serial, true => .skipBusy
  | .latestOnly, true => .supersedeThenStart
  | .parallel, _ => .start
  | .serial, false => .start
  | .latestOnly, false => .start

/-- A `serial` Trigger never runs two of its commands at once. -/
theorem serial_never_overlaps :
    admit .serial true = .skipBusy := rfl

/-- A `latest_only` fire never leaves an older run going beside its own. -/
theorem latest_only_interrupts_before_start (running : Bool)
    (h : admit .latestOnly running ≠ .start) : admit .latestOnly running = .supersedeThenStart := by
  cases running <;> simp_all [admit]

/-- An idle Trigger launches its command unless it asks for a queue. -/
theorem idle_starts (concurrency : ConcurrencyMode) (h : concurrency ≠ .queuedSerial) :
    admit concurrency false = .start := by
  cases concurrency <;> simp_all [admit]

/-- Only `parallel` launches beside a run that is still going. -/
theorem overlap_only_when_parallel (concurrency : ConcurrencyMode)
    (h : admit concurrency true = .start) : concurrency = .parallel := by
  cases concurrency <;> simp_all [admit]

end Triggers.CommandRun

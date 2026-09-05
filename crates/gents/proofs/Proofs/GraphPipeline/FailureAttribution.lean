import Proofs.GraphPipeline

/-!
# GraphRun failure-attribution refinement (#1375)

Projection of the EXISTING GraphRun owner: `run` is GraphPipeline.Run,
`primary` is its error's abstract cause ID, and `generation` is update_generation.
A `witness` is supplied only after the owner reloads durable failure evidence
inside its transaction. It is not an arbitrary caller assertion. This refinement
adds no lifecycle status, request writer, scheduler, or failure-discovery policy.
CAS conflict/retry is represented by an unchanged snapshot on generation loss.
-/
namespace GraphPipeline.FailureAttribution

abbrev Cause := Nat

structure Snapshot where
  run : GraphPipeline.Run
  generation : Nat
  primary : Option Cause
  deriving DecidableEq, Repr

/-- Commit the first fail-fast cause before performing sibling interrupts. -/
def capture (s : Snapshot) (expected : Nat) (witness : Option Cause) : Snapshot :=
  if s.run.status = .running ∧ s.run.cancellationRequested = false ∧
      s.generation = expected then
    match s.primary, witness with
    | none, some cause => { s with primary := some cause, generation := s.generation + 1 }
    | _, _ => s
  else s

/-- This is only failure-driven interruption; user cancellation has its own intent. -/
def mayInterruptForFailure (s : Snapshot) : Bool :=
  s.run.status == .running && !s.run.cancellationRequested && s.primary.isSome

/-- Existing durable cancellation intent. It takes priority over a pinned failure. -/
def requestCancel (s : Snapshot) : Snapshot :=
  if s.run.status = .running ∧ s.run.cancellationRequested = false then
    { s with run := { s.run with cancellationRequested := true }
             generation := s.generation + 1 }
  else s

/-- Existing terminal CAS, restricted to failure/cancellation projection.
Success and failure-discovery eligibility remain in the parent GraphPipeline
contract. `allTerminal` is reloaded in the terminal transaction. A direct failure
with no active work may install its first cause atomically with failed status. -/
def finish (s : Snapshot) (expected : Nat) (allTerminal : Bool)
    (witness : Option Cause) : Snapshot :=
  if s.run.status = .running ∧ s.generation = expected ∧ allTerminal = true then
    if s.run.cancellationRequested then
      { s with run := { s.run with status := .cancelled }
               primary := none, generation := s.generation + 1 }
    else
      match s.primary.orElse (fun _ => witness) with
      | none => s
      | some cause =>
          { s with run := { s.run with status := .failed }
                   primary := some cause, generation := s.generation + 1 }
  else s

theorem capture_preserves_lifecycle (s : Snapshot) (expected : Nat) (witness : Option Cause) :
    (capture s expected witness).run = s.run := by
  unfold capture
  split
  · cases s.primary <;> cases witness <;> rfl
  · rfl

theorem first_cause_cannot_be_replaced (s : Snapshot) (cause : Cause)
    (h : s.primary = some cause) (expected : Nat) (witness : Option Cause) :
    capture s expected witness = s := by
  simp [capture, h]

theorem losing_capture_is_noop (s : Snapshot) (expected : Nat) (witness : Option Cause)
    (h : s.generation ≠ expected) : capture s expected witness = s := by
  simp [capture, h]

/-- Compose the actual generation-fenced capture with its ensuing interruption
projection. The snapshot supplied here is the durable transaction/reload view. -/
def captureAndObserve (s : Snapshot) (expected : Nat) (witness : Option Cause) :
    Snapshot × Bool :=
  let next := capture s expected witness
  (next, mayInterruptForFailure next)

theorem capture_loser_observes_winner (durable : Snapshot) (expected : Nat)
    (witness : Option Cause) (h : durable.generation ≠ expected) :
    captureAndObserve durable expected witness = (durable, mayInterruptForFailure durable) := by
  simp [captureAndObserve, losing_capture_is_noop durable expected witness h]

theorem cancelled_capture_never_requests_failure_interrupt (durable : Snapshot)
    (expected : Nat) (witness : Option Cause)
    (h : durable.run.cancellationRequested = true) :
    captureAndObserve durable expected witness = (durable, false) := by
  simp [captureAndObserve, capture, mayInterruptForFailure, h]

theorem first_capture_records_witness (s : Snapshot) (cause : Cause)
    (h_running : s.run.status = .running)
    (h_cancel : s.run.cancellationRequested = false) (h_empty : s.primary = none) :
    (capture s s.generation (some cause)).primary = some cause ∧
      (capture s s.generation (some cause)).generation = s.generation + 1 := by
  simp [capture, h_running, h_cancel, h_empty]

theorem losing_terminal_cas_is_noop (s : Snapshot) (expected : Nat)
    (allTerminal : Bool) (witness : Option Cause) (h : s.generation ≠ expected) :
    finish s expected allTerminal witness = s := by
  simp [finish, h]

theorem cancellation_blocks_failure_capture (s : Snapshot)
    (h : s.run.cancellationRequested = true) (expected : Nat) (witness : Option Cause) :
    capture s expected witness = s := by
  simp [capture, h]

theorem interruption_requires_committed_cause (s : Snapshot)
    (h : mayInterruptForFailure s = true) : s.primary.isSome = true := by
  simp [mayInterruptForFailure, Bool.and_eq_true] at h
  exact h.2

theorem active_work_prevents_terminalization (s : Snapshot) (expected : Nat)
    (witness : Option Cause) : finish s expected false witness = s := by
  simp [finish]

theorem failed_projection_keeps_pinned_cause (s : Snapshot) (cause : Cause)
    (h_running : s.run.status = .running) (h_cancel : s.run.cancellationRequested = false)
    (h_cause : s.primary = some cause) (witness : Option Cause) :
    (finish s s.generation true witness).run.status = .failed ∧
      (finish s s.generation true witness).primary = some cause := by
  simp [finish, h_running, h_cancel, h_cause, Option.orElse]

theorem explicit_cancellation_wins (s : Snapshot)
    (h_running : s.run.status = .running) (h_cancel : s.run.cancellationRequested = true)
    (witness : Option Cause) :
    (finish s s.generation true witness).run.status = .cancelled ∧
      (finish s s.generation true witness).primary = none := by
  simp [finish, h_running, h_cancel]

theorem terminal_snapshot_cannot_change (s : Snapshot)
    (h : s.run.status ≠ .running) (expected : Nat) (witness : Option Cause)
    (allTerminal : Bool) :
    capture s expected witness = s ∧ requestCancel s = s ∧
      finish s expected allTerminal witness = s := by
  simp [capture, requestCancel, finish, h]

inductive Event where
  /-- Another durable request failed; observing it does not write the GraphRun latch. -/
  | observedFailure (cause : Cause)
  | capture (expected : Nat) (witness : Option Cause)
  | cancel
  | finish (expected : Nat) (allTerminal : Bool) (witness : Option Cause)
  deriving DecidableEq, Repr

def step (s : Snapshot) : Event → Snapshot
  | .observedFailure _ => s
  | .capture expected witness => capture s expected witness
  | .cancel => requestCancel s
  | .finish expected allTerminal witness => finish s expected allTerminal witness

/-- Includes every post-event snapshot so Rust can fence order and CAS losers. -/
def trace (s : Snapshot) : List Event → List Snapshot
  | [] => []
  | event :: rest => let next := step s event; next :: trace next rest

def initial : Snapshot :=
  { run := { runId := 1, graphId := 1, revisionId := 1, revisionDigest := 1
             status := .running, seedCommitted := true
             cancellationRequested := false, resultsCommitted := false }
    generation := 0, primary := none }

end GraphPipeline.FailureAttribution

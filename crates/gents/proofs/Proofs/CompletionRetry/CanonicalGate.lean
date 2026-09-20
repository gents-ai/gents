import Proofs.CompletionRetry.CanonicalExecution
import Proofs.CanonicalOutput.Execution.Gate

/-!
# Retry policy at the canonical execution gate

This is the application-facing adapter for provider-attempt closure decisions.
It reuses the existing execution gate and retry policy rather than introducing a
second scheduler.  Its deliberately narrow operation type prevents application
code from choosing the raw execution `accept`/`retract` operations at this
boundary.
-/

namespace CompletionRetry.CanonicalGate

open CanonicalOutput

abbrev Actor := CanonicalOutput.Execution.Gate.Actor

structure State where
  gate : CanonicalOutput.Execution.Gate.State
  retry : CompletionRetry.State
  deriving DecidableEq

inductive Operation where
  | retract (generation : Nat) (closing : Segment)
  | accept (generation : Nat) (closing : Segment) (message : MessageEnvelope)
      (targets : List CanonicalOutput.Execution.RemoteTarget)
      (admissions : List CanonicalOutput.Execution.ToolAdmission)
  deriving DecidableEq

inductive Error where
  | clock
  | request
  | source
  | policy
  | gate
  deriving DecidableEq, Repr

def requestCoherent (state : State) : Bool :=
  state.retry.request == state.gate.execution.requestId

def atTime (state : CompletionRetry.State) (now : Time) : Option CompletionRetry.State :=
  if state.now ≤ now then some { state with now := now } else none

def acceptedReplay (retry : CompletionRetry.State) (message : MessageEnvelope) : Bool :=
  match retry.phase with
  | .accepted header | .acceptedToolFailed header => header == message.header.id
  | _ => false

def retractionReplay (retry : CompletionRetry.State) : Bool :=
  match retry.phase with
  | .retracted _ _ _ => true
  | _ => false

private def optionOr {α : Type} (error : Error) : Option α → Except Error α
  | none => .error error
  | some value => .ok value

def gateOperation : Operation → CanonicalOutput.Execution.Gate.Operation
  | .retract generation closing => .retract generation closing
  | .accept generation closing message targets admissions =>
      .accept generation closing message targets admissions

def policyStep (retry : CompletionRetry.State) : Operation → Except Error CompletionRetry.State
  | .retract _ closing =>
      if !CanonicalExecution.sourceMatches retry closing then .error .source
      else match CompletionRetry.step? retry (.confirmRetraction true) with
      | some post => .ok post
      | none => if retractionReplay retry then .ok retry else .error .policy
  | .accept _ closing message _ _ =>
      if !CanonicalExecution.sourceMatches retry closing then .error .source
      else match CompletionRetry.step? retry (.accept message.header.id) with
      | some post => .ok post
      | none => if acceptedReplay retry message then .ok retry else .error .policy

theorem policyStep_preserves_now (before after : CompletionRetry.State) (operation : Operation)
    (h : policyStep before operation = .ok after) : after.now = before.now := by
  cases operation with
  | retract generation closing =>
      simp only [policyStep] at h
      split at h <;> try contradiction
      next hsource =>
        cases hp : before.phase <;> simp [policyStep, CompletionRetry.step?,
          retractionReplay, hp] at h
        all_goals cases h; rfl
  | accept generation closing message targets admissions =>
      simp only [policyStep] at h
      split at h <;> try contradiction
      next hsource =>
        cases hp : before.phase <;> simp [policyStep, CompletionRetry.step?,
          acceptedReplay, hp] at h
        all_goals try split at h <;> try contradiction
        all_goals cases h; rfl

/-- Commit a provider closure decision against the latest gate-held execution
world and the retry policy at the same authoritative time.  A replay retains the
already-advanced retry state while still asking the execution owner to validate
the exact immutable fact. -/
def commit (state : State) (actor : Actor) (now : Time) (operation : Operation) :
    Except Error State :=
  if !requestCoherent state then .error .request
  else match atTime state.retry now with
  | none => .error .clock
  | some observedRetry => match policyStep observedRetry operation with
    | .error error => .error error
    | .ok retry => match CanonicalOutput.Execution.Gate.commit state.gate actor now
        (gateOperation operation) with
      | none => .error .gate
      | some gate => .ok ⟨gate, retry⟩

def policyActionAllowed : CompletionRetry.Action → Bool
  | .confirmRetraction _ | .accept _ => false
  | _ => true

abbrev PolicyOperation := { action : CompletionRetry.Action // policyActionAllowed action }

def policyClockAllowed (now : Time) (action : CompletionRetry.Action) : Bool :=
  match action with
  | .wake wakeAt => wakeAt == now
  | _ => true

/-- Policy-only progress never impersonates canonical acceptance or retraction.
It advances at a clock no older than either composed owner; the next closure
decision must still enter `commit` and its held execution gate. -/
def stepPolicy (state : State) (now : Time) (operation : PolicyOperation) : Option State := do
  if !requestCoherent state || now < state.gate.execution.lease.now ||
      !policyClockAllowed now operation.val then none
  let observed ← atTime state.retry now
  let retry ← CompletionRetry.step? observed operation.val
  some { state with retry := retry }

def acquire (state : State) (actor : Actor) (independent : Bool) : Option State := do
  let gate ← CanonicalOutput.Execution.Gate.acquire state.gate actor independent
  some { state with gate := gate }

def scheduling (state : State) (actor : Actor)
    (event : StorageWriteGate.Event) : Option State := do
  let gate ← CanonicalOutput.Execution.Gate.scheduling state.gate actor event
  some { state with gate := gate }

def gateOperationAllowed : CanonicalOutput.Execution.Gate.Operation → Bool
  | .accept .. | .retract .. => false
  | _ => true

abbrev GateOperation := { operation : CanonicalOutput.Execution.Gate.Operation //
  gateOperationAllowed operation }

/-- All non-closure execution work continues to use the existing gate. Provider
acceptance and retry retraction are unrepresentable here and must use `commit`. -/
def commitGate (state : State) (actor : Actor) (now : Time)
    (operation : GateOperation) : Option State := do
  if !requestCoherent state then none
  let gate ← CanonicalOutput.Execution.Gate.commit state.gate actor now operation.val
  some { state with gate := gate }

inductive Trace : State → State → Prop where
  | refl (state : State) : Trace state state
  | commit {before after : State} (actor : Actor) (now : Time) (operation : Operation)
      (h : commit before actor now operation = .ok after) : Trace before after
  | policy {before after : State} (now : Time) (operation : PolicyOperation)
      (h : stepPolicy before now operation = some after) : Trace before after
  | acquire {before after : State} (actor : Actor) (independent : Bool)
      (h : acquire before actor independent = some after) : Trace before after
  | scheduling {before after : State} (actor : Actor) (event : StorageWriteGate.Event)
      (h : scheduling before actor event = some after) : Trace before after
  | gate {before after : State} (actor : Actor) (now : Time) (operation : GateOperation)
      (h : commitGate before actor now operation = some after) : Trace before after
  | trans {first second third : State} : Trace first second → Trace second third →
      Trace first third

theorem successful_commit_is_actual_gate_commit {before after : State}
    (actor : Actor) (now : Time) (operation : Operation)
    (h : commit before actor now operation = .ok after) :
    CanonicalOutput.Execution.Gate.commit before.gate actor now (gateOperation operation) =
      some after.gate := by
  unfold commit at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  cases hg : CanonicalOutput.Execution.Gate.commit before.gate actor now
      (gateOperation operation) with
  | none => simp [hg] at h
  | some gate => simp [hg] at h; cases h; rfl

theorem commit_nextSequence_monotone {before after : State} (actor : Actor) (now : Time)
    (operation : Operation) (h : commit before actor now operation = .ok after) :
    before.gate.execution.transcript.nextSeq ≤ after.gate.execution.transcript.nextSeq := by
  unfold commit at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  cases hg : CanonicalOutput.Execution.Gate.commit before.gate actor now
      (gateOperation operation) with
  | none => simp [hg] at h
  | some gate =>
      simp [hg] at h
      cases h
      exact CanonicalOutput.Execution.Gate.successful_commit_nextSequence_monotone
        before.gate gate actor now (gateOperation operation) hg

theorem commit_synchronizes_retry_clock {before after : State} (actor : Actor) (now : Time)
    (operation : Operation) (h : commit before actor now operation = .ok after) :
    after.retry.now = now := by
  unfold commit at h
  split at h <;> try contradiction
  next hrequest =>
    cases ht : atTime before.retry now with
    | none => simp [ht] at h
    | some observed =>
      have hnow : observed.now = now := by
        unfold atTime at ht
        split at ht
        · cases ht; rfl
        · contradiction
      simp [ht] at h
      cases hp : policyStep observed operation with
      | error error => simp [hp] at h
      | ok retry =>
        cases hg : CanonicalOutput.Execution.Gate.commit before.gate actor now
            (gateOperation operation) with
        | none => simp [hp, hg] at h
        | some gate =>
          simp [hp, hg] at h
          cases h
          exact (policyStep_preserves_now observed retry operation hp).trans hnow

theorem stepPolicy_preserves_gate {before after : State} (now : Time)
    (operation : PolicyOperation) (h : stepPolicy before now operation = some after) :
    after.gate = before.gate := by
  unfold stepPolicy at h
  split at h <;> try contradiction
  cases ht : atTime before.retry now with
  | none => simp [ht] at h
  | some observed =>
    cases hp : CompletionRetry.step? observed operation.val with
    | none => simp [ht, hp] at h
    | some retry => simp [ht, hp] at h; cases h; rfl

theorem acquire_preserves_gate_world {before after : State} (actor : Actor)
    (independent : Bool) (h : acquire before actor independent = some after) :
    after.gate.execution = before.gate.execution := by
  unfold CanonicalGate.acquire at h
  cases hg : CanonicalOutput.Execution.Gate.acquire before.gate actor independent with
  | none => simp [hg] at h
  | some gate =>
    simp [hg] at h; cases h
    exact CanonicalOutput.Execution.Gate.acquire_preserves_durable_world
      before.gate gate actor independent hg

theorem scheduling_preserves_gate_world {before after : State} (actor : Actor)
    (event : StorageWriteGate.Event) (h : scheduling before actor event = some after) :
    after.gate.execution = before.gate.execution := by
  unfold CanonicalGate.scheduling at h
  cases hg : CanonicalOutput.Execution.Gate.scheduling before.gate actor event with
  | none => simp [hg] at h
  | some gate =>
    simp [hg] at h; cases h
    exact CanonicalOutput.Execution.Gate.scheduling_preserves_durable_world
      before.gate gate actor event hg

theorem commitGate_nextSequence_monotone {before after : State} (actor : Actor)
    (now : Time) (operation : GateOperation)
    (h : commitGate before actor now operation = some after) :
    before.gate.execution.transcript.nextSeq ≤ after.gate.execution.transcript.nextSeq := by
  unfold commitGate at h
  split at h <;> try contradiction
  cases hg : CanonicalOutput.Execution.Gate.commit before.gate actor now operation.val with
  | none => simp [hg] at h
  | some gate =>
    simp [hg] at h; cases h
    exact CanonicalOutput.Execution.Gate.successful_commit_nextSequence_monotone
      before.gate gate actor now operation.val hg

theorem Trace.nextSequence_monotone {before after : State} (trace : Trace before after) :
    before.gate.execution.transcript.nextSeq ≤ after.gate.execution.transcript.nextSeq := by
  induction trace with
  | refl => exact Nat.le_refl _
  | commit actor now operation h => exact commit_nextSequence_monotone actor now operation h
  | policy now operation h => rw [stepPolicy_preserves_gate now operation h]
  | acquire actor independent h => rw [acquire_preserves_gate_world actor independent h]
  | scheduling actor event h => rw [scheduling_preserves_gate_world actor event h]
  | gate actor now operation h => exact commitGate_nextSequence_monotone actor now operation h
  | trans left right ihLeft ihRight => exact Nat.le_trans ihLeft ihRight

end CompletionRetry.CanonicalGate

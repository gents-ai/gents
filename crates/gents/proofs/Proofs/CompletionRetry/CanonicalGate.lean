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

def requestCoherent (state : CanonicalOutput.Execution.World) : Bool :=
  state.retry.request == state.requestId

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

def policyStep (purpose : RequestPurpose) (retry : CompletionRetry.State) :
    Operation → Except Error CompletionRetry.State
  | .retract _ closing =>
      if !CanonicalExecution.sourceMatches purpose retry closing then .error .source
      else match CompletionRetry.step? retry (.confirmRetraction true) with
      | some post => .ok post
      | none => if retractionReplay retry then .ok retry else .error .policy
  | .accept _ closing message _ _ =>
      if !CanonicalExecution.sourceMatches purpose retry closing then .error .source
      else match CompletionRetry.step? retry (.accept message.header.id) with
      | some post => .ok post
      | none => if acceptedReplay retry message then .ok retry else .error .policy

theorem policyStep_preserves_now (purpose : RequestPurpose)
    (before after : CompletionRetry.State) (operation : Operation)
    (h : policyStep purpose before operation = .ok after) : after.now = before.now := by
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
def commit (state : CanonicalOutput.Execution.World) (actor : Actor) (now : Time)
    (operation : Operation) : Except Error CanonicalOutput.Execution.World :=
  if !requestCoherent state then .error .request
  else match atTime state.retry now with
  | none => .error .clock
  | some observedRetry => match policyStep state.purpose observedRetry operation with
    | .error error => .error error
    | .ok retry => match CanonicalOutput.Execution.Gate.commit state actor now
        (gateOperation operation) with
      | none => .error .gate
      | some world => .ok { world with retry := retry }

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
def stepPolicy (state : CanonicalOutput.Execution.World) (now : Time)
    (operation : PolicyOperation) : Option CanonicalOutput.Execution.World := do
  if !requestCoherent state || now < state.lease.now ||
      !policyClockAllowed now operation.val then none
  let observed ← atTime state.retry now
  let retry ← CompletionRetry.step? observed operation.val
  some { state with retry := retry }

def gateOperationAllowed : CanonicalOutput.Execution.Gate.Operation → Bool
  | .accept .. | .retract .. => false
  | _ => true

abbrev GateOperation := { operation : CanonicalOutput.Execution.Gate.Operation //
  gateOperationAllowed operation }

/-- All non-closure execution work continues to use the existing gate. Provider
acceptance and retry retraction are unrepresentable here and must use `commit`. -/
def commitGate (state : CanonicalOutput.Execution.World) (actor : Actor) (now : Time)
    (operation : GateOperation) : Option CanonicalOutput.Execution.World := do
  if !requestCoherent state then none
  CanonicalOutput.Execution.Gate.commit state actor now operation.val

theorem successful_commit_is_actual_gate_commit
    {before after : CanonicalOutput.Execution.World}
    (actor : Actor) (now : Time) (operation : Operation)
    (h : commit before actor now operation = .ok after) :
    CanonicalOutput.Execution.Gate.commit before actor now (gateOperation operation) =
      some { after with retry := before.retry } := by
  unfold commit at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  cases hg : CanonicalOutput.Execution.Gate.commit before actor now
      (gateOperation operation) with
  | none => simp [hg] at h
  | some gate =>
      have hretry := (CanonicalOutput.Execution.Gate.commit_preserves_composed_control
        before gate actor now (gateOperation operation) hg).2.2
      simp [hg] at h
      cases h
      congr 1
      cases gate
      simp_all

theorem commit_nextSequence_monotone {before after : CanonicalOutput.Execution.World}
    (actor : Actor) (now : Time)
    (operation : Operation) (h : commit before actor now operation = .ok after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  unfold commit at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  cases hg : CanonicalOutput.Execution.Gate.commit before actor now
      (gateOperation operation) with
  | none => simp [hg] at h
  | some gate =>
      simp [hg] at h
      cases h
      exact CanonicalOutput.Execution.Gate.successful_commit_nextSequence_monotone
        before gate actor now (gateOperation operation) hg

theorem commit_synchronizes_retry_clock {before after : CanonicalOutput.Execution.World}
    (actor : Actor) (now : Time)
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
      cases hp : policyStep before.purpose observed operation with
      | error error => simp [hp] at h
      | ok retry =>
        cases hg : CanonicalOutput.Execution.Gate.commit before actor now
            (gateOperation operation) with
        | none => simp [hp, hg] at h
        | some gate =>
          simp [hp, hg] at h
          cases h
          exact (policyStep_preserves_now before.purpose observed retry operation hp).trans hnow

theorem stepPolicy_preserves_gate {before after : CanonicalOutput.Execution.World} (now : Time)
    (operation : PolicyOperation) (h : stepPolicy before now operation = some after) :
    after = { before with retry := after.retry } := by
  unfold stepPolicy at h
  split at h <;> try contradiction
  cases ht : atTime before.retry now with
  | none => simp [ht] at h
  | some observed =>
    cases hp : CompletionRetry.step? observed operation.val with
    | none => simp [ht, hp] at h
    | some retry =>
        simp [ht, hp] at h
        cases h
        cases before
        simp

theorem commitGate_nextSequence_monotone
    {before after : CanonicalOutput.Execution.World} (actor : Actor)
    (now : Time) (operation : GateOperation)
    (h : commitGate before actor now operation = some after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  unfold commitGate at h
  split at h <;> try contradiction
  cases hg : CanonicalOutput.Execution.Gate.commit before actor now operation.val with
  | none => simp [hg] at h
  | some gate =>
    simp [hg] at h; cases h
    exact CanonicalOutput.Execution.Gate.successful_commit_nextSequence_monotone
      before after actor now operation.val hg

end CompletionRetry.CanonicalGate

import Proofs.CompletionRetry.Properties
import Proofs.CanonicalOutput.Execution.Transition
import Proofs.CanonicalOutput.Execution.Properties
import Proofs.CanonicalOutput.Execution.Examples

/-!
# Retry policy / canonical execution composition

The policy machine cannot assert durable retraction or accepted publication by
supplying a Boolean or message id. This wrapper advances those policy states
only in the same pure transition result in which CanonicalOutput.Execution has
accepted the exact keyed source mutation.

`usageCharged` is a monotone observation from the existing InferenceCall
accounting owner. These composed transitions preserve that observation; charge
completeness and replay idempotency remain obligations of that owner.
-/
namespace CompletionRetry.CanonicalExecution

open CanonicalOutput

structure World where
  execution : CanonicalOutput.Execution.World
  retry : CompletionRetry.State
  deriving DecidableEq

inductive Error where
  | wrongSource
  | policyRejected
  | execution (error : CanonicalOutput.Execution.Error)
  deriving DecidableEq, Repr

def expectedCoordinate (state : CompletionRetry.State) : Coordinate :=
  ⟨state.request, .provider state.scope state.turn state.attempt⟩

def sourceMatches (state : CompletionRetry.State) (closing : Segment) : Bool :=
  closing.coordinate == expectedCoordinate state

/-- A retry becomes schedulable only after the exact attempt source acquires a
durable Retracted closure through the execution owner. -/
def retractBeforeRetry (world : World) (generation : Nat) (closing : Segment) :
    Except Error World :=
  if sourceMatches world.retry closing = false then .error .wrongSource
  else match CompletionRetry.step? world.retry (.confirmRetraction true) with
    | none => .error .policyRejected
    | some retry => match CanonicalOutput.Execution.retractBeforeRetry
        world.execution generation closing with
      | .error error => .error (.execution error)
      | .ok execution => .ok ⟨execution, retry⟩

/-- Acceptance is injected into retry policy only by successful atomic canonical
publication of this exact provider attempt. -/
def acceptAndPublish (world : World) (generation : Nat) (closing : Segment)
    (message : MessageEnvelope)
    (targets : List CanonicalOutput.Execution.RemoteTarget)
    (admissions : List CanonicalOutput.Execution.ToolAdmission) : Except Error World :=
  if sourceMatches world.retry closing = false then .error .wrongSource
  else match CompletionRetry.step? world.retry (.accept message.header.id) with
    | none => .error .policyRejected
    | some retry => match CanonicalOutput.Execution.acceptAndPublish
        world.execution generation closing message targets admissions with
      | .error error => .error (.execution error)
      | .ok execution => .ok ⟨execution, retry⟩

theorem retraction_success_records_policy_and_execution
    {before after : World} {generation : Nat} {closing : Segment}
    (h : retractBeforeRetry before generation closing = .ok after) :
    sourceMatches before.retry closing = true ∧
      CompletionRetry.step? before.retry (.confirmRetraction true) = some after.retry ∧
      CanonicalOutput.Execution.retractBeforeRetry before.execution generation closing =
        .ok after.execution := by
  unfold retractBeforeRetry at h
  split at h
  · contradiction
  · rename_i hsource
    cases hp : CompletionRetry.step? before.retry (.confirmRetraction true) with
    | none => simp [hp] at h
    | some retry =>
        cases he : CanonicalOutput.Execution.retractBeforeRetry
            before.execution generation closing with
        | error error => simp [hp, he] at h
        | ok execution =>
            simp [hp, he] at h
            cases h
            exact ⟨Bool.eq_true_of_not_eq_false hsource, rfl, rfl⟩

theorem acceptance_success_is_canonical
    {before after : World} {generation : Nat} {closing : Segment}
    {message : MessageEnvelope} {targets : List CanonicalOutput.Execution.RemoteTarget}
    {admissions : List CanonicalOutput.Execution.ToolAdmission}
    (h : acceptAndPublish before generation closing message targets admissions = .ok after) :
    sourceMatches before.retry closing = true ∧
      CompletionRetry.step? before.retry (.accept message.header.id) = some after.retry ∧
      CanonicalOutput.Execution.acceptAndPublish before.execution generation closing
        message targets admissions = .ok after.execution := by
  unfold acceptAndPublish at h
  split at h
  · contradiction
  · rename_i hsource
    cases hp : CompletionRetry.step? before.retry (.accept message.header.id) with
    | none => simp [hp] at h
    | some retry =>
        cases he : CanonicalOutput.Execution.acceptAndPublish before.execution generation
            closing message targets admissions with
        | error error => simp [hp, he] at h
        | ok execution =>
            simp [hp, he] at h
            cases h
            exact ⟨Bool.eq_true_of_not_eq_false hsource, rfl, rfl⟩

theorem retraction_success_preserves_accounted_usage
    {before after : World} {generation : Nat} {closing : Segment}
    (h : retractBeforeRetry before generation closing = .ok after) :
    after.retry.usageCharged = before.retry.usageCharged :=
  CompletionRetry.retraction_preserves_usage
    (retraction_success_records_policy_and_execution h).2.1

theorem acceptance_success_preserves_accounted_usage
    {before after : World} {generation : Nat} {closing : Segment}
    {message : MessageEnvelope} {targets : List CanonicalOutput.Execution.RemoteTarget}
    {admissions : List CanonicalOutput.Execution.ToolAdmission}
    (h : acceptAndPublish before generation closing message targets admissions = .ok after) :
    after.retry.usageCharged = before.retry.usageCharged :=
  CompletionRetry.acceptance_preserves_usage
    (acceptance_success_is_canonical h).2.1

namespace Examples

def retryAwaitingRetraction : CompletionRetry.State :=
  { request := 10, scope := 0, turn := 0
    phase := .retractRequired .transport "io" 12
    budget := { transportRetries := 3, resampleRetries := 2, allowRepair := true }
    transportUsed := 0, resampleUsed := 0, repairUsed := false
    lastParseError := none, now := 6, deadline := none, attempt := 0
    usageCharged := 0 }

def completeClose : Segment :=
  CanonicalOutput.Execution.Examples.emptyClose 100 0 .complete 5

def acceptedCompleteWorld : World :=
  { execution := CanonicalOutput.Execution.Examples.world 6 [completeClose]
    retry := retryAwaitingRetraction }

/-- A byte-identical Complete closure is an acceptance fact, not a retry
retraction replay. The composed transition returns no successor retry state. -/
theorem accepted_complete_cannot_confirm_retry_retraction :
    (match retractBeforeRetry acceptedCompleteWorld 7 completeClose with
      | .error (.execution .invalidSegment) => true
      | _ => false) = true := by
  native_decide

end Examples

end CompletionRetry.CanonicalExecution

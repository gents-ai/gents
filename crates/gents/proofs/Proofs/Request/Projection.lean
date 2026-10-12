import Proofs.Request.Transition

namespace Request.Projection

/-- Admission is concerned with the node/agent binding, not with how
    many replicated projection documents currently carry that binding. -/
def agentCompatible (requested : String) (persisted : List String) : Bool :=
  persisted.all (· == requested)

theorem duplicate_equal_bindings_are_compatible (requested : String) :
    agentCompatible requested [requested, requested] = true := by
  simp [agentCompatible]

theorem conflicting_binding_is_rejected (requested existing : String)
    (h : existing ≠ requested) :
    agentCompatible requested [requested, existing] = false := by
  simp [agentCompatible, h]

/-- Projection writes converge every replicated row's value. A missing
    projection is repaired by creating exactly one row; duplicates are updated
    in place rather than becoming an admission failure. -/
def projectAll {α : Type} (value : α) (rows : List α) : List α :=
  if rows.isEmpty then [value] else rows.map fun _ => value

theorem projection_is_present_after_write {α : Type} (value : α) (rows : List α) :
    projectAll value rows ≠ [] := by
  cases rows <;> simp [projectAll]

theorem duplicate_projection_values_converge {α : Type} (oldA oldB value : α) :
    projectAll value [oldA, oldB] = [value, value] := by
  simp [projectAll]

/-- A deterministic projection failure (for example, a missing repairable
    projection) may fall back to the authoritative request edge. A retryable
    storage failure must preserve atomic ownership and re-attempt the request
    and projection together. -/
inductive ProjectionAttempt where
  | committed
  | deterministicFailure
  | retryableFailure
  deriving DecidableEq

def terminalCommitAfterProjectionAttempt : ProjectionAttempt → Bool
  | .committed => true
  | .deterministicFailure => true
  | .retryableFailure => false

theorem terminal_request_commits_after_projection_error :
    terminalCommitAfterProjectionAttempt .deterministicFailure = true := by
  rfl

theorem retryable_projection_failure_preserves_atomic_retry :
    terminalCommitAfterProjectionAttempt .retryableFailure = false := by
  rfl

/-- The native owned-execution fence commits Processing before any provider-capable
work, including title dispatch and pre-inference compaction. Only the live owner's
terminal transaction may certify interruption before that fence. Missing certificates
and Processing interruptions cannot be inferred from absent provider rows on replicas. -/
def stoppedBeforeRunning (owned : Bool) (source target : RequestState) : Bool :=
  owned && source == .claimed && target == .interrupted

theorem claim_only_follows_pending {pre post : RequestContext}
    (transition : RequestContext.Transition pre post) (claimed : post.state = .claimed) :
    pre.state = .pending := by
  cases transition <;> simp_all

theorem processing_cannot_certify_no_provider (owned : Bool) (target : RequestState) :
    stoppedBeforeRunning owned .processing target = false := by
  simp [stoppedBeforeRunning]

theorem recovery_cannot_certify_no_provider (source target : RequestState) :
    stoppedBeforeRunning false source target = false := by
  simp [stoppedBeforeRunning]

theorem claimed_owner_certifies_no_provider :
    stoppedBeforeRunning true .claimed .interrupted = true := by
  rfl

end Request.Projection

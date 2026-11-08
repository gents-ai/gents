import Proofs.Request.Transition

namespace Request.Projection

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

/-- The native owned-execution fence commits Processing before any provider-capable work (title dispatch, pre-inference compaction); only the live owner's terminal transaction certifies interruption, and missing certificates are not inferable from absent provider rows on replicas. -/
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

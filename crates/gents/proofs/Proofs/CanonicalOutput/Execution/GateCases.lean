import Proofs.CanonicalOutput.Execution.Gate
import Proofs.CanonicalOutput.Execution.Examples

namespace CanonicalOutput.Execution.Gate.Cases

open CanonicalOutput.Execution.Examples

def flushBeforeRecovery : Option Bool := do
  let held ← acquire (initial (world 5)) 1 true
  let appended ← commit held 1 9 (.append 7 (raw 100 0 0 9))
  let blocked := (acquire appended 2 true).isNone
  let released ← scheduling appended 1 .release
  let recoveryHeld ← acquire released 2 true
  let items := [RecoveryItem.mk (partialClose 101 0 1 10) (some (recoveryMessage 200 101 0 10))]
  pure (blocked && (commit recoveryHeld 2 10 (.recover 7 8 5 20 items)).isNone)

/-- The later recovery must see the committed flush's renewal; acquisition and
release cannot restore an expired pre-flush snapshot. -/
theorem producer_first_renewal_blocks_recovery : flushBeforeRecovery = some true := by native_decide

def recoveryBeforeStaleWriter : Option Bool := do
  let held ← acquire (initial (world 5)) 1 true
  let appended ← commit held 1 5 (.append 7 (raw 100 0 0 5))
  let released ← scheduling appended 1 .release
  let recoveryHeld ← acquire released 2 true
  let items := [RecoveryItem.mk (partialClose 101 0 1 10) (some (recoveryMessage 200 101 0 10))]
  let recovered ← commit recoveryHeld 2 10 (.recover 7 8 5 20 items)
  let releasedAgain ← scheduling recovered 2 .release
  let staleHeld ← acquire releasedAgain 1 true
  let late : Segment := { raw 102 0 1 11 with flush := some ⟨1, [⟨0, 1, none⟩], [66]⟩ }
  pure ((commit staleHeld 1 11 (.append 7 late)).isNone &&
    recovered.execution.currentGeneration? == some 8)

theorem recovery_first_blocks_stale_writer : recoveryBeforeStaleWriter = some true := by native_decide

def sameTaskWaitDoesNotCommit : Option Bool := do
  let held ← acquire (initial (world 5)) 1 false
  let suspended ← scheduling held 1 .siblingWait
  pure ((commit suspended 1 5 (.append 7 (raw 100 0 0 5))).isNone &&
    suspended.execution.segments.isEmpty)

theorem suspended_holder_cannot_publish : sameTaskWaitDoesNotCommit = some true := by native_decide

/-- The truncation counterexample is reached through admitted raw appends,
not by assuming an arbitrary malformed initial collection. -/
def completeTruncationRejected : Option Bool := do
  let first ← acquire (initial (world 5)) 1 true
  let firstCommitted ← commit first 1 5 (.append 7 providerFirstFlush)
  let firstReleased ← scheduling firstCommitted 1 .release
  let second ← acquire firstReleased 1 true
  let secondCommitted ← commit second 1 5 (.append 7 providerSecondFlush)
  let secondReleased ← scheduling secondCommitted 1 .release
  let publication ← acquire secondReleased 1 true
  pure ((commit publication 1 5
    (.accept 7 shortProviderClose shortProviderMessage [])).isNone)

theorem admitted_flushes_cannot_be_silently_truncated :
    completeTruncationRejected = some true := by native_decide

end CanonicalOutput.Execution.Gate.Cases

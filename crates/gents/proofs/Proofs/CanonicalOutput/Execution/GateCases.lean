import Proofs.CanonicalOutput.Execution.Gate
import Proofs.CanonicalOutput.Execution.Examples

namespace CanonicalOutput.Execution.Gate.Cases

open CanonicalOutput.Execution.Examples

def renewalBeforeRecovery : Option Bool := do
  let held ← acquire (initial (world 5)) 1 true
  let renewed ← commit held 1 8 (.renew 7 10)
  let blocked := (acquire renewed 2 true).isNone
  let released ← scheduling renewed 1 .release
  let recoveryHeld ← acquire released 2 true
  pure (blocked && (commit recoveryHeld 2 10 (.recover 7 8 5 20 [])).isNone)

/-- Recovery sees the explicit renewal winner; acquisition and release cannot
restore the expired pre-renewal deadline. -/
theorem explicit_renewal_winner_blocks_recovery :
    renewalBeforeRecovery = some true := by native_decide

def outputAloneDoesNotRenew : Option Bool := do
  let held ← acquire (initial (world 5)) 1 true
  let appended ← commit held 1 5 (.append 7 (raw 100 0 0 5))
  let released ← scheduling appended 1 .release
  let recoveryHeld ← acquire released 2 true
  let items := [RecoveryItem.mk (partialClose 101 0 1 10)
    (some (recoveryMessage 200 101 0 10))]
  let recovered ← commit recoveryHeld 2 10 (.recover 7 8 5 20 items)
  pure (recovered.execution.currentGeneration? == some 8)

theorem producer_output_alone_expires : outputAloneDoesNotRenew = some true := by
  native_decide

/-- Under the scheduling premise that the owner of a published, dispatched tool
intent reaches its held write gate at the due time, it renews explicitly before
yielding. This composes the transcript intent, not the external tool scheduler,
and uses no tool output as heartbeat evidence. -/
def toolWaitExplicitRenewal : Option Bool := do
  let acceptHeld ← acquire (initial (routedWorld 5)) 1 true
  let accepted ← commit acceptHeld 1 5 (.accept 7 providerTurn providerMessage [remote])
  let acceptReleased ← scheduling accepted 1 .release
  let dispatchHeld ← acquire acceptReleased 1 true
  let dispatched ← commit dispatchHeld 1 5 (.dispatch 7 permit)
  let dispatchReleased ← scheduling dispatched 1 .release
  let renewalHeld ← acquire dispatchReleased 1 false
  let renewed ← commit renewalHeld 1 8 (.renew 7 10)
  let released ← scheduling renewed 1 .release
  pure (renewed.execution.lease.lease == .active 7 5 13 &&
    renewed.execution.transcript.RunningPublishedCall 600 && released.owner.isNone)

theorem scheduled_tool_wait_can_renew_before_yield :
    toolWaitExplicitRenewal = some true := by native_decide

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

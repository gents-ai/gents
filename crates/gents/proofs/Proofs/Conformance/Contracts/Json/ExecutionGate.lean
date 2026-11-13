import Proofs.Runtime.CanonicalOutput.Execution.GateCases
import Proofs.Conformance.Family

namespace Conformance.ExecutionGateContracts

open CanonicalOutput.Execution.Gate.Cases

structure Case where
  name : String
  observed : Option Bool

def cases : List Case :=
  [ ⟨"foreground_accept_dispatch_close_deliver_terminal", foregroundEndToEnd⟩
  , ⟨"foreground_complete_while_running_rejected", foregroundCompletionWhileRunningRejected⟩
  , ⟨"explicit_background_parent_terminal_late_delivery", backgroundLateDelivery⟩
  , ⟨"recovery_cancels_pending_and_blocks_remote_dispatch", recoveryCancelsPendingAtomically⟩
  , ⟨"recovery_hands_off_running_without_fake_stop", recoveryHandsOffRunning⟩
  , ⟨"foreign_request_same_generation_unchanged", some foreignSameGenerationUnchanged⟩
  , ⟨"delivery_recovery_race_renewal_wins", renewalBeforeRecovery⟩
  , ⟨"stale_writer_loses_after_recovery", recoveryBeforeStaleWriter⟩ ]
  ++ [ ⟨"producer_output_does_not_renew", outputAloneDoesNotRenew⟩
     , ⟨"foreground_tool_wait_explicitly_renews", toolWaitExplicitRenewal⟩
     , ⟨"suspended_holder_cannot_publish", sameTaskWaitDoesNotCommit⟩
     , ⟨"complete_extent_cannot_be_truncated", completeTruncationRejected⟩
     , ⟨"spawned_background_lost_ack_replays", some spawnedAdmissionReplayCases⟩
     , ⟨"corrupt_payload_revocation_preserves_facts", some corruptRevocationCases⟩ ]

example : cases.all (fun value => value.observed == some true) = true := by
  native_decide

structure SummaryInput where
  name : String
  deriving Lean.ToJson

structure SummaryOutput where
  completed : Bool
  deriving Lean.ToJson

/-- model-production-gap: each summary folds a whole modeled application
trace (several requests, tools and recoveries) into one witness; production
has no single owner that executes a whole trace, so the scripts of
`native_execution_contracts_script` bind its steps instead. -/
@[conformance_family]
def traceSummaryUnreached : Family SummaryInput SummaryOutput where
  inputs := cases.map fun value => { name := value.name }
  model := fun i => { completed := (cases.find? (·.name == i.name)).any (·.observed == some true) }

end Conformance.ExecutionGateContracts
